use super::*;

/// The largest slot buffer, and the most buffers, an ended activation keeps for
/// the next block.
const SPARE_BYTES: usize = 1 << 16;
const SPARE_BUFFERS: usize = 256;

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// Run one activation of `f`. Its slots are frame memory: they end when the
    /// activation does, by return or by failure, so a later use of an address into
    /// them is a stop (crate docs, "Values, slots and addresses").
    #[inline(never)]
    pub(super) fn activation(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<()> {
        let Some(b) = self.m.functions[f.index()].body.as_ref() else {
            // A bodiless function reached through a value: a closed-list function
            // (LANG-20) or a foreign one, which runs only natively.
            let m = self.m;
            let definition = &m.instances[m.functions[f.index()].instance.index()].definition;
            match hirsel_core_ir::CLOSED
                .iter()
                .position(|entry| definition.is_core(entry))
            {
                Some(i) => H::closed(self, i as u8),
                None => H::bodiless(self, t, f),
            }
            unreachable!("external functions do not run here");
        };
        // Measure from inside this shared entry so the root and child dispatch
        // frames below it do not affect the guest allowance.
        if t.at.is_empty() {
            let base = 0u8;
            TASK_STACK_BASE.with(|stack| stack.set(&base as *const u8 as usize));
        }
        let l = self.lowered(f, b);
        let mut slots = self.take_words();
        let frame_bytes = match &l {
            Some(l) => {
                self.frame_slots(&mut slots, l.slot_sizes());
                l.slot_sizes().iter().sum::<u64>()
            }
            None => {
                let mut bytes = 0u64;
                for s in &b.slots {
                    let size = self.place_size(*s);
                    bytes += size;
                    slots.push(self.block(size, None, 0, None));
                }
                bytes
            }
        };
        TASK_FRAME_BYTES.with(|live| live.set(live.get() + frame_bytes));
        self.enter_frame(t, f);
        let r = match l {
            Some(l) => self.flat_blocks(t, &l, b, &slots, flat::Args::Words(args)),
            None => self.blocks(t, b, args, &slots),
        };
        t.at.pop();
        self.free_slots(slots);
        TASK_FRAME_BYTES.with(|live| live.set(live.get() - frame_bytes));
        r
    }

    /// FAIL-17: refuse the entered activation with a `Stack` cause when the
    /// frame slots or the evaluator's activation stack pass the build's
    /// allowance. The first refusal releases the margin for cleanup, and
    /// an `isolate` restores it. `enter` checks the engine reserve separately.
    pub(super) fn stack_check(&mut self, t: &mut Task) -> X<()> {
        use hirsel_core_layout::stack::{ALLOWANCE, MARGIN};
        let frame = 0u8;
        let released = TASK_STACK_RELEASED.with(std::cell::Cell::get);
        let base = TASK_STACK_BASE.with(std::cell::Cell::get);
        let used = base.abs_diff(&frame as *const u8 as usize);
        let allowance = if released {
            ALLOWANCE + MARGIN
        } else {
            ALLOWANCE
        };
        let slots = TASK_FRAME_BYTES.with(std::cell::Cell::get);
        if slots <= allowance && used <= allowance as usize {
            return Ok(());
        }
        TASK_STACK_RELEASED.with(|r| r.set(true));
        let site = self.site(t);
        self.record(
            t,
            Cause {
                kind: "Stack",
                message: [0, 0, 0],
                site,
            },
        );
        Err(Stop::Chained)
    }

    /// An activation is entered: FAIL-15 when the task's stack has no room left
    /// and the activation limit end the entire run, bypassing every program
    /// cleanup; then the host's `enter_call`.
    #[inline(always)]
    pub(super) fn enter(&mut self, t: &Task) {
        let frame = 0u8;
        let exhausted = TASK_STACK_ROOM.with(|room| {
            let (base, available) = room.get();
            base.abs_diff(&frame as *const u8 as usize) > available
        });
        if exhausted {
            std::panic::resume_unwind(Box::new(RunStop("Stack")));
        }
        if t.at.len() >= self.activations {
            std::panic::resume_unwind(Box::new(RunStop("Activations")));
        }
        H::enter_call(self, t, &frame as *const u8 as usize);
    }

    /// A frame slot of each size, pushed to `slots`.
    pub(super) fn frame_slots(&mut self, slots: &mut Vec<u64>, sizes: &[u64]) {
        for &size in sizes {
            slots.push(self.block(size, None, 0, None));
        }
    }

    /// `f`'s activation's site record, popped when it ends.
    pub(super) fn enter_frame(&mut self, t: &mut Task, f: FuncId) {
        if !t.step_bounds.is_empty() && t.at.iter().any(|here| here.f == f) {
            let definition =
                &self.m.instances[self.m.functions[f.index()].instance.index()].definition;
            if !definition.package_is("core", "1") {
                self.step_floor += 1;
            }
        }
        t.at.push(Here {
            f,
            at: Pos::Entry,
            inherits: self.inherits[f.index()],
        });
    }

    /// An activation's slots end with it.
    pub(super) fn free_slots(&mut self, slots: Vec<u64>) {
        for &p in &slots {
            let bytes = self.mem.remove(split(p).0).bytes;
            if bytes.capacity() <= SPARE_BYTES && self.spare.len() < SPARE_BUFFERS {
                self.spare.push(bytes);
            }
        }
        self.words.push(slots);
    }
    pub(super) fn blocks(&mut self, t: &mut Task, b: &Body, args: &[u64], slots: &[u64]) -> X<()> {
        let depth = t.limits.len();
        let reserved = t.reserved.len();
        let mut vals = self.take_words();
        vals.resize(b.values.len(), 0);
        let mut vectors = simd::Vectors::default();
        let mut places = self.places.pop().unwrap_or_default();
        places.clear();
        places.resize(b.addresses.len(), (0u64, At::Other));
        let mut frame_charges = HashMap::new();
        let pool = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap();
        let mut block = BlockId(0);
        let mut incoming = self.take_words();
        incoming.extend_from_slice(args);
        let mut outgoing = self.take_words();
        let r = 'blocks: loop {
            assert!(
                self.instructions != 0,
                "evaluator instruction budget exhausted"
            );
            self.instructions -= 1;
            let blk = &b.blocks[block.index()];
            vectors.unpack(b, &mut vals, pool(blk.params), &incoming);
            let first = blk.insts.start as usize;
            for (k, inst) in blk.insts.get(&b.insts).unwrap().iter().enumerate() {
                if matches!(inst, Inst::LoopIteration) {
                    self.step_floor += u64::from(!t.step_bounds.is_empty());
                    continue;
                }
                if matches!(inst, Inst::BuiltinUse { .. }) {
                    #[cfg(feature = "map-observation")]
                    if let Inst::BuiltinUse { definition } = inst
                        && let Some(stats) = &mut self.maps.observed
                    {
                        if definition.is_core("map.group_match") {
                            stats.groups += 1;
                        } else if definition.is_core("map.set_strong") {
                            stats.promotions += 1;
                            stats.rebuild_promotions += u64::from(stats.rebuilding);
                        } else if stats.promoted_in_rebuild && definition.is_core("map.install") {
                            stats.resumed_installs += 1;
                        }
                    }
                    continue;
                }
                assert!(
                    self.instructions != 0,
                    "evaluator instruction budget exhausted"
                );
                self.instructions -= 1;
                t.at.last_mut().unwrap().at = Pos::Inst((first + k) as u32);
                // Keep the large instruction dispatcher off the recursive call stack.
                let r = match *inst {
                    Inst::Call { call } => self.call_inst(
                        t,
                        b,
                        &mut vals,
                        &mut vectors,
                        &places,
                        &b.calls[call.index()],
                    ),
                    _ => self.inst(
                        t,
                        b,
                        slots,
                        &mut vals,
                        &mut vectors,
                        &mut places,
                        &mut frame_charges,
                        inst,
                    ),
                };
                if let Err(stop) = r {
                    self.stopped(t, stop);
                    match inst.fail(b) {
                        Some(fb) => {
                            block = fb;
                            incoming.clear();
                            continue 'blocks;
                        }
                        None => break 'blocks Err(Stop::Chained),
                    }
                }
            }
            let edge = match &b.terminators[block.index()] {
                Terminator::Jump(e) => e,
                Terminator::Branch { condition, yes, no } => {
                    if vals[condition.index()] != 0 {
                        yes
                    } else {
                        no
                    }
                }
                Terminator::Switch {
                    value,
                    arms,
                    otherwise,
                } => {
                    let x = vals[value.index()];
                    let arms = arms.get(&b.pools.arms).unwrap();
                    arms.iter()
                        .find(|a| a.value == x)
                        .map_or(otherwise, |a| &a.edge)
                }
                Terminator::Return(r) => {
                    assert_eq!(t.limits.len(), depth, "a return left a bound scope open");
                    assert_eq!(t.reserved.len(), reserved, "a return left an isolate open");
                    self.results.clear();
                    for &id in pool(*r) {
                        vectors.push(b, &vals, id, &mut self.results);
                    }
                    break 'blocks Ok(());
                }
                Terminator::Raise { cause: c, cleanup } => {
                    match self.raise(t, b, slots, &vals, &places, block, c, *cleanup) {
                        Ok(c) => {
                            block = c;
                            incoming.clear();
                            continue 'blocks;
                        }
                        Err(stop) => break 'blocks Err(stop),
                    }
                }
                Terminator::Propagate => break 'blocks Err(Stop::Chained),
                Terminator::Unreachable => panic!("an unreachable point was reached"),
            };
            let edge = &b.edges[edge.index()];
            outgoing.clear();
            for &id in pool(edge.args) {
                vectors.push(b, &vals, id, &mut outgoing);
            }
            std::mem::swap(&mut incoming, &mut outgoing);
            block = edge.target;
        };
        if r.is_err() {
            assert_eq!(t.limits.len(), depth, "a failure left a bound scope open");
            assert_eq!(t.reserved.len(), reserved, "a failure left an isolate open");
        }
        self.words.extend([vals, incoming, outgoing]);
        self.places.push(places);
        r
    }

    /// An instruction stopped: a new cause is recorded at the activation's site.
    pub(super) fn stopped(&mut self, t: &mut Task, stop: Stop) {
        if let Stop::Cause(c) = stop {
            let site = self.site(t);
            let cause = Cause {
                kind: c,
                message: [0; 3],
                site,
            };
            self.record(t, cause);
        }
    }

    /// Block `block`'s `raise`: its cause is recorded at the block's site, and the
    /// run goes on in `cleanup`, or the failure leaves the activation.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn raise(
        &mut self,
        t: &mut Task,
        b: &Body,
        slots: &[u64],
        vals: &[u64],
        places: &[(u64, At)],
        block: BlockId,
        c: &RaiseCause,
        cleanup: Option<BlockId>,
    ) -> X<BlockId> {
        let mut message = [0; 3];
        match c {
            RaiseCause::Fail(a) => {
                let (p, _) = self.address(b, slots, vals, places, *a)?;
                message = [0, 8, 16].map(|o| self.load(p + o, 8));
            }
            // `Capacity(c)`'s payload sits where `Fail`'s carrier does.
            RaiseCause::Capacity {
                requested,
                available,
            } => message = [vals[requested.index()], vals[available.index()], 0],
            RaiseCause::Unwrap | RaiseCause::Bounds => {}
        }
        t.at.last_mut().unwrap().at = Pos::Block(block.0);
        let site = self.site(t);
        let cause = Cause {
            kind: cause(*c),
            message,
            site,
        };
        self.record(t, cause);
        cleanup.ok_or(Stop::Chained)
    }
    pub(super) fn frame_budget(
        &mut self,
        t: &Task,
        vals: &mut [u64],
        frame_charges: &mut HashMap<AddressId, u64>,
        inst: &Inst,
    ) {
        match *inst {
            Inst::FrameAdmit {
                storage,
                admitted,
                available,
                charge,
                overflow,
                replace,
            } => {
                let limit = t.limits.last().unwrap().clone();
                let charge = vals[charge.index()];
                match self.admit_charge(
                    &limit,
                    charge,
                    overflow.is_some_and(|v| vals[v.index()] != 0),
                ) {
                    Err(room) => (vals[admitted.index()], vals[available.index()]) = (0, room),
                    Ok(()) => {
                        if replace {
                            self.refund(limit, frame_charges[&storage]);
                        }
                        frame_charges.insert(storage, charge);
                        (vals[admitted.index()], vals[available.index()]) = (1, 0);
                    }
                }
            }
            Inst::FrameActive { storage, result } => {
                vals[result.index()] = u64::from(frame_charges.contains_key(&storage));
            }
            Inst::FrameRefund { storage } => {
                let limit = t.limits.last().unwrap().clone();
                self.refund(limit, frame_charges.remove(&storage).unwrap());
            }
            _ => unreachable!(),
        }
    }
}
