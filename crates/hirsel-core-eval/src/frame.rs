use super::*;

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// Run one activation of `f`. Its slots are frame memory: they end when the
    /// activation does, by return or by failure, so a later use of an address into
    /// them is a stop (crate docs, "Values, slots and addresses").
    pub(super) fn activation(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<()> {
        let b = self.m.functions[f.index()]
            .body
            .as_ref()
            .expect("external functions do not run here");
        let mut slots = self.take_words();
        for s in &b.slots {
            slots.push(self.block(self.place_size(*s), None, 0, None));
        }
        let r = self.frame(t, f, b, args, &slots);
        for &p in &slots {
            self.mem.remove(split(p).0);
        }
        self.words.push(slots);
        r
    }
    pub(super) fn frame(
        &mut self,
        t: &mut Task,
        f: FuncId,
        b: &Body,
        args: &[u64],
        slots: &[u64],
    ) -> X<()> {
        t.at.push(Here {
            f,
            at: Pos::Entry,
            inherits: self.inherits[f.index()],
        });
        let r = self.blocks(t, b, args, slots);
        t.at.pop();
        r
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
