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
//! differential oracle (`samples_behave_as_derived_by_hand`).
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
    /// By edge, its target and its moves' range in `moves`, and whether a
    /// destination is also a source, so the moves go through a buffer.
    edges: Vec<Edge>,
    /// Edge moves as (destination, source) value registers.
    moves: Vec<(u32, u32)>,
}

struct Edge {
    target: u32,
    moves: (u32, u32),
    overlap: bool,
}

/// An operation; `at` is the instruction it runs, for its site and failure edge.
/// Operand types are resolved at lowering; each specialised arm does what the
/// tree walker's `inst` arm does for that instruction, in the same order.
#[derive(Clone, Copy)]
enum Op {
    /// The tree walker's instruction `at`.
    Generic {
        at: u32,
    },
    Call {
        at: u32,
        call: u32,
    },
    Step {
        at: u32,
    },
    Const {
        at: u32,
        result: u32,
        bits: u64,
    },
    /// A `Binary` over integers of type `ty`.
    Int {
        at: u32,
        result: u32,
        op: BinaryOp,
        ty: Ty,
        left: u32,
        right: u32,
    },
    /// A `Compare` of integers of type `ty`.
    Compare {
        at: u32,
        result: u32,
        op: CompareOp,
        ty: Ty,
        left: u32,
        right: u32,
    },
    Address {
        at: u32,
        address: u32,
    },
    /// A `Read` of an `n`-byte scalar.
    Read {
        at: u32,
        result: u32,
        address: u32,
        n: u32,
    },
    Pointer {
        at: u32,
        result: u32,
        address: u32,
    },
    Convert {
        at: u32,
        result: u32,
        flag: Option<u32>,
        operand: u32,
        mode: ConvertMode,
        from: Ty,
        to: Ty,
    },
    /// A `Store` of a scalar of type `ty`.
    Store {
        at: u32,
        address: u32,
        value: u32,
        ty: Ty,
    },
}

impl Op {
    fn at(self) -> u32 {
        match self {
            Op::Generic { at }
            | Op::Call { at, .. }
            | Op::Step { at }
            | Op::Const { at, .. }
            | Op::Int { at, .. }
            | Op::Compare { at, .. }
            | Op::Address { at, .. }
            | Op::Read { at, .. }
            | Op::Store { at, .. }
            | Op::Pointer { at, .. }
            | Op::Convert { at, .. } => at,
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

fn lower(b: &Body) -> Option<Lowered> {
    if b.values.iter().any(|t| matches!(t, Ty::Vector(_))) {
        return None;
    }
    let ty = |v: ValueId| b.values[v.index()];
    let mut ops = Vec::with_capacity(b.insts.len());
    let mut blocks = Vec::with_capacity(b.blocks.len());
    for blk in &b.blocks {
        let start = ops.len() as u32;
        let first = blk.insts.start;
        for (k, inst) in blk.insts.get(&b.insts).unwrap().iter().enumerate() {
            let at = first + k as u32;
            ops.push(match *inst {
                // A builtin's use marks the code it came from; it does not run.
                Inst::BuiltinUse { .. } => continue,
                Inst::Call { call } => Op::Call { at, call: call.0 },
                Inst::Step { .. } => Op::Step { at },
                Inst::Const { result, bits } => Op::Const {
                    at,
                    result: result.0,
                    bits,
                },
                Inst::Binary {
                    result,
                    op,
                    left,
                    right,
                    ..
                } if !ty(left).is_float() => Op::Int {
                    at,
                    result: result.0,
                    op,
                    ty: ty(left),
                    left: left.0,
                    right: right.0,
                },
                Inst::Compare {
                    result,
                    op,
                    left,
                    right,
                } if !ty(left).is_float() => Op::Compare {
                    at,
                    result: result.0,
                    op,
                    ty: ty(left),
                    left: left.0,
                    right: right.0,
                },
                Inst::Address { address, .. } => Op::Address {
                    at,
                    address: address.0,
                },
                Inst::Read {
                    result, address, ..
                } => Op::Read {
                    at,
                    result: result.0,
                    address: address.0,
                    n: bytes_of(ty(result)) as u32,
                },
                Inst::Pointer { result, address } => Op::Pointer {
                    at,
                    result: result.0,
                    address: address.0,
                },
                Inst::Convert {
                    result,
                    flag,
                    operand,
                    mode,
                } => Op::Convert {
                    at,
                    result: result.0,
                    flag: flag.map(|f| f.0),
                    operand: operand.0,
                    mode,
                    from: ty(operand),
                    to: ty(result),
                },
                Inst::Store { address, value } => Op::Store {
                    at,
                    address: address.0,
                    value: value.0,
                    ty: ty(value),
                },
                _ => Op::Generic { at },
            });
        }
        blocks.push((start, ops.len() as u32));
    }
    let mut moves = Vec::new();
    let edges = b
        .edges
        .iter()
        .map(|e| {
            let params = b.blocks[e.target.index()]
                .params
                .get(&b.pools.values)
                .unwrap();
            let args = e.args.get(&b.pools.values).unwrap();
            let start = moves.len() as u32;
            moves.extend(params.iter().zip(args).map(|(p, a)| (p.0, a.0)));
            let these = &moves[start as usize..];
            let overlap = these
                .iter()
                .any(|&(d, _)| these.iter().any(|&(_, s)| s == d));
            Edge {
                target: e.target.0,
                moves: (start, moves.len() as u32),
                overlap,
            }
        })
        .collect();
    Some(Lowered {
        ops,
        blocks,
        edges,
        moves,
    })
}

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// `f`'s body lowered, when this run is flat and the body can be.
    pub(super) fn lowered(&self, f: FuncId, b: &Body) -> Option<Arc<Lowered>> {
        #[cfg(feature = "map-observation")]
        if self.maps.observed.is_some() {
            return None;
        }
        self.flat.as_ref()?[f.index()]
            .get_or_init(|| lower(b).map(Arc::new))
            .clone()
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
                    Op::Const { result, bits, .. } => {
                        vals[result as usize] = bits;
                        continue;
                    }
                    Op::Int {
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
                            None => {
                                let division = matches!(op, BinaryOp::Div | BinaryOp::Rem);
                                Err(Stop::Cause(if division && c == 0 {
                                    "Division"
                                } else {
                                    "Overflow"
                                }))
                            }
                        }
                    }
                    Op::Pointer {
                        result, address, ..
                    } => {
                        vals[result as usize] = places[address as usize].0;
                        continue;
                    }
                    Op::Convert {
                        at,
                        result,
                        flag,
                        operand,
                        mode,
                        from,
                        to,
                    } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
                        let (r, fl) = convert(mode, from, to, vals[operand as usize]);
                        vals[result as usize] = r;
                        if let Some(f) = flag {
                            vals[f as usize] = fl;
                        }
                        continue;
                    }
                    Op::Compare {
                        result,
                        op,
                        ty,
                        left,
                        right,
                        ..
                    } => {
                        let ord = int(ty, vals[left as usize]).cmp(&int(ty, vals[right as usize]));
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
                    Op::Address { at, address } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
                        let a = AddressId(address);
                        self.address(b, slots, &vals, &places, a)
                            .map(|place| places[address as usize] = place)
                    }
                    Op::Read {
                        at,
                        result,
                        address,
                        n,
                    } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
                        let (p, held) = places[address as usize];
                        self.touch(AddressId(address), p, n as usize, false);
                        vals[result as usize] = self.read_scalar(p, n as usize);
                        if let At::Word(w, size) = held {
                            self.check_carrier(p - w.offset(), size, None);
                        }
                        continue;
                    }
                    Op::Store {
                        at,
                        address,
                        value,
                        ty,
                    } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
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
                    Op::Step { at } => {
                        if t.step_bounds.is_empty() {
                            self.steps = self.steps.saturating_add(1);
                            Ok(())
                        } else {
                            t.at.last_mut().unwrap().at = Pos::Inst(at);
                            self.take_steps(t, 1)
                        }
                    }
                    Op::Call { at, call } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
                        self.call_inst(
                            t,
                            b,
                            &mut vals,
                            &mut vectors,
                            &places,
                            &b.calls[call as usize],
                        )
                    }
                    Op::Generic { at } => {
                        t.at.last_mut().unwrap().at = Pos::Inst(at);
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
                    t.at.last_mut().unwrap().at = Pos::Inst(at);
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
