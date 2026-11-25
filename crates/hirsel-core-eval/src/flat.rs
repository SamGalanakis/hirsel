//! The flat engine (COMPTIME-DIRECTION 3.2, E3): a body's checked IR lowered,
//! on its function's first call, to one linear array of operations per body,
//! run by a dispatch loop over the activation's dense value registers.
//!
//! It is the same IR, not a second meaning of it. An operation is either one the
//! loop runs itself, with its operands resolved at lowering, or `Generic`, which
//! hands the instruction to the tree walker's `inst` over the same registers,
//! places and frame state. Block entry, edges, failure edges, raises, the
//! executed-instruction count, steps, charges, the activation bound and failure
//! sites are the tree walker's; the tree walker stays as this engine's
//! differential oracle (`samples_behave_as_derived_by_hand` and the corpus).
//!
//! What lowering resolves:
//! - An address's steps. What a step's place holds ([`At`]) follows from its
//!   base's alone, so only pointer arithmetic, a `deref`'s load and an element's
//!   bounds check are left to run. An address whose steps do not type, where the
//!   tree walker's `address` stops the run, keeps `address`.
//! - An integer `Binary` or `Compare` runs on u64 or i64; a `Binary` of another
//!   type keeps the tree walker's i128 arithmetic.
//! - A call's argument and result registers.
//!
//! The activation's site is written before the operations that read it (a call,
//! a generic instruction, a step under a bound) and when an operation stops.
//!
//! Only a body with no vector value is lowered; a body with one runs on the tree
//! walker, whose edge words carry vectors.
use super::*;

/// One body lowered.
pub(super) struct Lowered {
    /// Every block's operations, block after block.
    ops: Vec<Op>,
    /// By block, its operations' range in `ops`.
    blocks: Vec<(u32, u32)>,
    edges: Vec<Edge>,
    /// Edge moves as (destination, source) value registers.
    moves: Vec<(u32, u32)>,
    /// By address, what its place holds when it is resolved.
    ats: Vec<At>,
    /// The resolved addresses with pieces: each one's base and its pieces' range
    /// in `pieces`.
    addresses: Vec<(Base, (u32, u32))>,
    pieces: Vec<Piece>,
    calls: Vec<Call>,
    /// The calls' arguments.
    args: Vec<Source>,
    /// The calls' result registers.
    results: Vec<u32>,
}

/// An edge's target and its moves' range in `moves`, and whether a destination
/// is also a source, so the moves go through a buffer.
struct Edge {
    target: u32,
    moves: (u32, u32),
    overlap: bool,
}

#[derive(Clone, Copy)]
enum Base {
    Slot(u32),
    Value(u32),
    Place(u32),
}

/// One run-time piece of an address, after its base, in step order.
#[derive(Clone, Copy)]
enum Piece {
    /// Field, case field and seq word offsets, summed.
    Offset(u64),
    /// The pointer stored at the place; a counted node's payload is 8 bytes on.
    Deref { counted: bool },
    /// An inline array element: `Bounds` unless the index is below `len`.
    Element { index: u32, len: u64, stride: u64 },
    /// `index` units of `size` bytes further on (`Index`, and `Byte` at 1).
    Scaled { index: u32, size: u64 },
}

/// How an address is resolved.
#[derive(Clone, Copy)]
enum Resolution {
    Slot(u32),
    Value(u32),
    /// `offset` bytes into another address's place.
    Field(u32, u64),
    /// An index into `Lowered::addresses`.
    Pieces(u32),
}

struct Call {
    callee: Callee,
    signature: SigId,
    args: (u32, u32),
    results: (u32, u32),
}

#[derive(Clone, Copy)]
enum Source {
    Value(u32),
    Place(u32),
}

/// An operation. One that can stop, or reads the site, carries `at`, the
/// instruction it runs.
#[derive(Clone, Copy)]
enum Op {
    /// The tree walker's instruction `at`.
    Generic {
        at: u32,
    },
    /// `call` indexes `Lowered::calls`.
    Call {
        at: u32,
        call: u32,
    },
    Step {
        at: u32,
    },
    Const {
        result: u32,
        bits: u64,
    },
    /// A `Binary` over integers of `width` bits.
    Int {
        at: u32,
        result: u32,
        op: BinaryOp,
        signed: bool,
        width: u8,
        left: u32,
        right: u32,
    },
    /// A `Binary` over another non-float type, on the tree walker's arithmetic.
    Wide {
        at: u32,
        result: u32,
        op: BinaryOp,
        ty: Ty,
        left: u32,
        right: u32,
    },
    /// A `Compare` of non-floats. A signed one compares its operands shifted
    /// left by `shift` as i64, which orders them as their values do.
    Compare {
        result: u32,
        op: CompareOp,
        signed: bool,
        shift: u8,
        left: u32,
        right: u32,
    },
    /// An address that is a slot.
    Slot {
        address: u32,
        slot: u32,
    },
    /// An address that is a pointer value.
    Value {
        address: u32,
        value: u32,
    },
    /// An address `offset` bytes into another's place.
    Field {
        address: u32,
        place: u32,
        offset: u64,
    },
    /// An address with pieces; `resolved` indexes `Lowered::addresses`.
    Address {
        at: u32,
        address: u32,
        resolved: u32,
    },
    /// An address left to the tree walker's `address`.
    Unresolved {
        at: u32,
        address: u32,
    },
    /// A `Read` of an `n`-byte scalar.
    Read {
        result: u32,
        address: u32,
        n: u8,
    },
    Pointer {
        result: u32,
        address: u32,
    },
    Convert {
        result: u32,
        /// The flag's register, or `NO_FLAG`.
        flag: u32,
        operand: u32,
        mode: ConvertMode,
        from: Ty,
        to: Ty,
    },
    /// A `Store` of a scalar of type `ty`.
    Store {
        address: u32,
        value: u32,
        ty: Ty,
    },
    /// `StorageLive` of a slot of `size` bytes.
    Live {
        slot: u32,
        size: u64,
    },
    Dead {
        slot: u32,
    },
}

const NO_FLAG: u32 = u32::MAX;

// The dispatch loop reads one operation per instruction: a wider one costs every
// instruction (round 2's `Option<u32>` flag made it 32 bytes, crc +5%).
const _: () = assert!(std::mem::size_of::<Op>() <= 24);

impl Op {
    /// The instruction a stop is at; only operations that can stop are asked.
    fn at(self) -> u32 {
        match self {
            Op::Generic { at }
            | Op::Call { at, .. }
            | Op::Step { at }
            | Op::Int { at, .. }
            | Op::Wide { at, .. }
            | Op::Address { at, .. }
            | Op::Unresolved { at, .. } => at,
            _ => unreachable!("this operation does not stop"),
        }
    }
}

/// The per-function lowering cache of a flat run.
pub(super) type Cache = Vec<std::cell::OnceCell<Option<Arc<Lowered>>>>;

pub(super) fn cache(m: &Module) -> Cache {
    m.functions
        .iter()
        .map(|_| std::cell::OnceCell::new())
        .collect()
}

mod lower;
use lower::int_fast;

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// `f`'s body lowered, when this run is flat and the body can be.
    pub(super) fn lowered(&self, f: FuncId, b: &Body) -> Option<Arc<Lowered>> {
        #[cfg(feature = "map-observation")]
        if self.maps.observed.is_some() {
            return None;
        }
        self.flat.as_ref()?[f.index()]
            .get_or_init(|| self.lower(b).map(Arc::new))
            .clone()
    }

    /// The pointer of resolved address `r`, as `address` computes it.
    fn place_of(
        &self,
        l: &Lowered,
        r: u32,
        slots: &[u64],
        vals: &[u64],
        places: &[(u64, At)],
    ) -> X<u64> {
        let (base, (start, end)) = l.addresses[r as usize];
        let mut p = match base {
            Base::Slot(s) => slots[s as usize],
            Base::Value(v) => vals[v as usize],
            Base::Place(x) => places[x as usize].0,
        };
        for &piece in &l.pieces[start as usize..end as usize] {
            match piece {
                Piece::Offset(off) => p += off,
                Piece::Deref { counted: false } => p = self.load(p, 8),
                Piece::Deref { counted: true } => p = self.load(p, 8) + 8,
                Piece::Element { index, len, stride } => {
                    let i = vals[index as usize];
                    if i >= len {
                        return Err(Stop::Cause("Bounds"));
                    }
                    p += i * stride;
                }
                Piece::Scaled { index, size } => p += vals[index as usize] * size,
            }
        }
        Ok(p)
    }

    /// Call `c` of `l`: `call_inst` with the call's registers resolved.
    fn flat_call(
        &mut self,
        t: &mut Task,
        l: &Lowered,
        c: &Call,
        vals: &mut [u64],
        places: &[(u64, At)],
    ) -> X<()> {
        let mut args = self.take_words();
        args.extend(
            l.args[c.args.0 as usize..c.args.1 as usize]
                .iter()
                .map(|&s| match s {
                    Source::Value(v) => vals[v as usize],
                    Source::Place(a) => places[a as usize].0,
                }),
        );
        let held = self.frozen.len();
        if self.checks {
            self.lend(&self.m.signatures[c.signature.index()], &args);
        }
        let r = match c.callee {
            Callee::Direct(f) => self.call(t, f, &args),
            Callee::Indirect(v) => self.call(t, FuncId((vals[v.index()] - FN_BASE) as u32), &args),
            Callee::Runtime(op) => self.rt(t, op, &args).map(|r| {
                self.results.clear();
                self.results.extend_from_slice(&r);
            }),
        };
        self.frozen.truncate(held);
        self.words.push(args);
        r?;
        let results = &l.results[c.results.0 as usize..c.results.1 as usize];
        for (&d, &w) in results.iter().zip(&self.results) {
            vals[d as usize] = w;
        }
        Ok(())
    }

    /// The tree walker's `blocks`, run over `l`.
    pub(super) fn flat_blocks(
        &mut self,
        t: &mut Task,
        l: &Lowered,
        b: &Body,
        args: &[u64],
        slots: &[u64],
    ) -> X<()> {
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
        for (p, &a) in pool(b.blocks[0].params).iter().zip(args) {
            vals[p.index()] = a;
        }
        let site = |t: &mut Task, at: u32| t.at.last_mut().unwrap().at = Pos::Inst(at);
        let mut block = 0usize;
        let r = 'blocks: loop {
            assert!(
                self.instructions != 0,
                "evaluator instruction budget exhausted"
            );
            self.instructions -= 1;
            let (start, end) = l.blocks[block];
            for &op in &l.ops[start as usize..end as usize] {
                assert!(
                    self.instructions != 0,
                    "evaluator instruction budget exhausted"
                );
                self.instructions -= 1;
                let r = match op {
                    Op::Const { result, bits } => {
                        vals[result as usize] = bits;
                        continue;
                    }
                    Op::Int {
                        result,
                        op,
                        signed,
                        width,
                        left,
                        right,
                        ..
                    } => {
                        let (a, c) = (vals[left as usize], vals[right as usize]);
                        match int_fast(op, signed, u32::from(width), a, c) {
                            Some(x) => {
                                vals[result as usize] = x;
                                continue;
                            }
                            None => Err(Stop::Cause(
                                if matches!(op, BinaryOp::Div | BinaryOp::Rem) && c == 0 {
                                    "Division"
                                } else {
                                    "Overflow"
                                },
                            )),
                        }
                    }
                    Op::Wide {
                        result,
                        op,
                        ty,
                        left,
                        right,
                        ..
                    } => {
                        let (a, c) = (vals[left as usize], vals[right as usize]);
                        match int_binary(ty, op, a, c) {
                            Some(x) => {
                                vals[result as usize] = x;
                                continue;
                            }
                            None => Err(Stop::Cause(
                                if matches!(op, BinaryOp::Div | BinaryOp::Rem) && c == 0 {
                                    "Division"
                                } else {
                                    "Overflow"
                                },
                            )),
                        }
                    }
                    Op::Compare {
                        result,
                        op,
                        signed,
                        shift,
                        left,
                        right,
                    } => {
                        let (a, c) = (vals[left as usize], vals[right as usize]);
                        let ord = if signed {
                            ((a << shift) as i64).cmp(&((c << shift) as i64))
                        } else {
                            a.cmp(&c)
                        };
                        use std::cmp::Ordering::*;
                        vals[result as usize] = u64::from(match op {
                            CompareOp::Eq => ord == Equal,
                            CompareOp::Ne => ord != Equal,
                            CompareOp::Lt => ord == Less,
                            CompareOp::Le => ord != Greater,
                            CompareOp::Gt => ord == Greater,
                            CompareOp::Ge => ord != Less,
                        });
                        continue;
                    }
                    Op::Slot { address, slot } => {
                        let a = address as usize;
                        places[a] = (slots[slot as usize], l.ats[a]);
                        continue;
                    }
                    Op::Value { address, value } => {
                        let a = address as usize;
                        places[a] = (vals[value as usize], l.ats[a]);
                        continue;
                    }
                    Op::Field {
                        address,
                        place,
                        offset,
                    } => {
                        let a = address as usize;
                        places[a] = (places[place as usize].0 + offset, l.ats[a]);
                        continue;
                    }
                    Op::Address {
                        address, resolved, ..
                    } => self
                        .place_of(l, resolved, slots, &vals, &places)
                        .map(|p| places[address as usize] = (p, l.ats[address as usize])),
                    Op::Unresolved { address, .. } => self
                        .address(b, slots, &vals, &places, AddressId(address))
                        .map(|place| places[address as usize] = place),
                    Op::Read { result, address, n } => {
                        let (p, held) = places[address as usize];
                        self.touch(AddressId(address), p, n as usize, false);
                        vals[result as usize] = self.read_scalar(p, n as usize);
                        if let At::Word(w, size) = held {
                            self.check_carrier(p - w.offset(), size, None);
                        }
                        continue;
                    }
                    Op::Pointer { result, address } => {
                        vals[result as usize] = places[address as usize].0;
                        continue;
                    }
                    Op::Convert {
                        result,
                        flag,
                        operand,
                        mode,
                        from,
                        to,
                    } => {
                        let (r, fl) = convert(mode, from, to, vals[operand as usize]);
                        vals[result as usize] = r;
                        if flag != NO_FLAG {
                            vals[flag as usize] = fl;
                        }
                        continue;
                    }
                    Op::Store { address, value, ty } => {
                        let mut x = vals[value as usize];
                        if let Some(k) = self.scrambled.as_mut()
                            && ty.is_float()
                            && float(ty, x).is_nan()
                        {
                            x = scrambled(ty, *k);
                            *k += 1;
                        }
                        let (p, held) = places[address as usize];
                        let n = bytes_of(ty);
                        self.touch(AddressId(address), p, n, true);
                        self.store(p, n, x);
                        if let At::Word(w, size) = held {
                            self.check_carrier(p - w.offset(), size, Some(w));
                        }
                        continue;
                    }
                    // `inst`'s `StorageLive` and `StorageDead`.
                    Op::Live { slot, size } => {
                        let blk = &mut self.mem[split(slots[slot as usize]).0];
                        blk.bytes.clear();
                        blk.bytes.resize(size as usize, 0);
                        blk.marks.clear();
                        blk.live = true;
                        continue;
                    }
                    Op::Dead { slot } => {
                        let blk = &mut self.mem[split(slots[slot as usize]).0];
                        blk.live = false;
                        blk.bytes.clear();
                        blk.marks.clear();
                        continue;
                    }
                    Op::Step { at } => {
                        if t.step_bounds.is_empty() {
                            self.steps = self.steps.saturating_add(1);
                            continue;
                        }
                        site(t, at);
                        self.take_steps(t, 1)
                    }
                    Op::Call { at, call } => {
                        site(t, at);
                        self.flat_call(t, l, &l.calls[call as usize], &mut vals, &places)
                    }
                    Op::Generic { at } => {
                        site(t, at);
                        self.inst(
                            t,
                            b,
                            slots,
                            &mut vals,
                            &mut vectors,
                            &mut places,
                            &mut frame_charges,
                            &b.insts[at as usize],
                        )
                    }
                };
                if let Err(stop) = r {
                    let at = op.at();
                    site(t, at);
                    self.stopped(t, stop);
                    match b.insts[at as usize].fail(b) {
                        Some(fb) => {
                            block = fb.index();
                            continue 'blocks;
                        }
                        None => break 'blocks Err(Stop::Chained),
                    }
                }
            }
            let edge = match &b.terminators[block] {
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
                    self.results
                        .extend(pool(*r).iter().map(|id| vals[id.index()]));
                    break 'blocks Ok(());
                }
                Terminator::Raise { cause: c, cleanup } => {
                    match self.raise(
                        t,
                        b,
                        slots,
                        &vals,
                        &places,
                        BlockId(block as u32),
                        c,
                        *cleanup,
                    ) {
                        Ok(c) => {
                            block = c.index();
                            continue 'blocks;
                        }
                        Err(stop) => break 'blocks Err(stop),
                    }
                }
                Terminator::Propagate => break 'blocks Err(Stop::Chained),
                Terminator::Unreachable => panic!("an unreachable point was reached"),
            };
            let e = &l.edges[edge.index()];
            let moves = &l.moves[e.moves.0 as usize..e.moves.1 as usize];
            if e.overlap {
                let mut words = self.take_words();
                words.extend(moves.iter().map(|&(_, s)| vals[s as usize]));
                for (&(d, _), &w) in moves.iter().zip(&words) {
                    vals[d as usize] = w;
                }
                self.words.push(words);
            } else {
                for &(d, s) in moves {
                    vals[d as usize] = vals[s as usize];
                }
            }
            block = e.target as usize;
        };
        if r.is_err() {
            assert_eq!(t.limits.len(), depth, "a failure left a bound scope open");
            assert_eq!(t.reserved.len(), reserved, "a failure left an isolate open");
        }
        self.words.push(vals);
        self.places.push(places);
        r
    }
}
