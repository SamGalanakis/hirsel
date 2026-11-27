//! The flat engine (COMPTIME-DIRECTION 3.2, E3): a body's checked IR lowered,
//! on its function's first call, to one linear array of operations per body,
//! run by a dispatch loop over the activation's dense value registers.
//!
//! It is the same IR, not a second meaning of it. An operation is either one the
//! loop runs itself, with its operands resolved at lowering, or `Generic`, which
//! hands the instruction to the tree walker's `inst` over the same registers,
//! places and frame state. Block entry, edges, failure edges, raises, the
//! executed-instruction count, steps, charges, the activation bound and failure
//! sites are the tree walker's. It is what a pure run, and so a constant's
//! initializer, runs on by default; the tree walker stays as its differential
//! oracle (`samples_behave_as_derived_by_hand` and the corpus).
//!
//! What lowering resolves:
//! - An address's steps. What a step's place holds ([`At`]) follows from its
//!   base's alone, so only pointer arithmetic, a `deref`'s load and an element's
//!   bounds check are left to run. An address whose steps do not type, where the
//!   tree walker's `address` stops the run, keeps `address`.
//! - An integer `Binary` or `Compare` runs on u64 or i64; a `Binary` of another
//!   type keeps the tree walker's i128 arithmetic.
//! - A call's argument and result registers.
//! - Terminators are operations, the last of their block's; a `Compare` whose
//!   result a block's `Branch` tests is fused into it.
//!
//! A constant is not an operation: a frame's registers start with every
//! constant's (a value is defined once, wf), and the operation after it counts
//! its instruction.
//!
//! The executed-instruction count is charged by run: on entry to a block, the
//! block's terminator and its instructions up to and including its first call
//! or generic instruction; after each of those returns, the instructions up to
//! the next. A run is what the tree walker charges unless an operation in it
//! stops, and then the rest of the run is given back (`Lowered::rest`). A run stop
//! unwinding out of a call or a generic instruction ends its run, so the count
//! it leaves is the tree walker's too.
//!
//! An exhausted budget stops where the tree walker's does. The tree walker
//! charges a block's terminator on entry and each instruction before it runs,
//! so the first instruction past the budget is the one that does not run. A
//! run longer than what is left takes all of it and owes the rest
//! (`Machine::owed`). Register arithmetic past that instruction cannot be
//! seen, so it runs on; the first memory access, call, generic instruction or
//! terminator at or past it (its `rest` below what is owed) panics before it
//! runs, and so does a stop past it. A stop before it gives back the rest of
//! its run less what is owed.
//!
//! The activation's site is written before the operations that read it (a call,
//! a generic instruction, a step under a bound) and when an operation stops.
//!
//! Only a body with no vector value is lowered; a body with one runs on the tree
//! walker, whose edge words carry vectors.
use super::*;

/// One body lowered.
pub(super) struct Lowered {
    /// Every block's operations, block after block, its terminator last.
    ops: Vec<Op>,
    /// By operation, the instructions after it to the end of its run: what is
    /// given back when it stops.
    rest: Vec<u32>,
    /// The registers a frame starts with: each constant's, and 0.
    init: Vec<u64>,
    /// By block, its first operation and what its entry charges.
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
    /// The calls' and returns' result registers.
    results: Vec<u32>,
    /// Block 0's parameter registers.
    params: Vec<u32>,
    /// By slot, its size (`place_size`).
    sizes: Vec<u64>,
    /// The body's value, address and slot counts, which [`lower::verify`] proved
    /// every register, place and slot index of the operations below.
    counts: Counts,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Counts {
    values: usize,
    addresses: usize,
    slots: usize,
}

/// An edge's target block's first operation and entry charge, its moves' range
/// in `moves`, and whether a destination is also a source, so the moves go
/// through a buffer.
struct Edge {
    start: u32,
    charge: u32,
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
pub(super) enum Source {
    Value(u32),
    Place(u32),
}

impl Source {
    /// The word this argument passes, from the caller's registers and places.
    #[inline(always)]
    fn of(self, vals: &[u64], places: &[(u64, At)]) -> u64 {
        match self {
            Source::Value(v) => get(vals, v),
            Source::Place(a) => get(places, a).0,
        }
    }
}

/// A frame's arguments: words, or a direct call's sources in its caller's
/// registers and places.
pub(super) enum Args<'a> {
    Words(&'a [u64]),
    Caller(&'a [Source], &'a [u64], &'a [(u64, At)]),
}

/// An operation. One that can stop, or reads the site, carries `at`, the
/// instruction it runs. One that ends a run carries `then`, what the next run
/// charges.
#[derive(Clone, Copy)]
enum Op {
    /// The tree walker's instruction `at`.
    Generic {
        at: u32,
        then: u32,
    },
    /// `call` indexes `Lowered::calls`.
    Call {
        at: u32,
        call: u32,
        then: u32,
    },
    Step {
        at: u32,
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
    /// `jump`; `edge` indexes `Lowered::edges`, as do the branches' edges.
    Jump {
        edge: u32,
    },
    Branch {
        condition: u32,
        yes: u32,
        no: u32,
    },
    /// A `Compare` and the `Branch` that tests its result, which is written too.
    CompareBranch {
        op: CompareOp,
        signed: bool,
        shift: u8,
        left: u32,
        right: u32,
        result: u32,
        yes: u32,
        no: u32,
    },
    /// `return` of the registers `results` ranges over in `Lowered::results`.
    Return {
        results: (u32, u32),
    },
    /// Block `block`'s `switch`, `raise`, `propagate` or `unreachable`.
    Exit {
        block: u32,
    },
}

const NO_FLAG: u32 = u32::MAX;

/// `Machine::settled`'s check, off the dispatch loop.
#[cold]
#[inline(never)]
fn owing(l: &Lowered, pc: usize, owed: u64) {
    assert!(
        u64::from(l.rest[pc]) >= owed,
        "evaluator instruction budget exhausted"
    );
}

// The dispatch loop reads one operation per instruction: a wider one costs every
// instruction (round 2's `Option<u32>` flag made it 32 bytes, crc +5%).
const _: () = assert!(std::mem::size_of::<Op>() <= 24);

impl Op {
    /// The instruction a stop is at; only operations that can stop are asked.
    fn at(self) -> u32 {
        match self {
            Op::Generic { at, .. }
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

/// The register `i` of `regs`.
#[inline(always)]
fn get<T: Copy>(regs: &[T], i: u32) -> T {
    debug_assert!((i as usize) < regs.len(), "register {i} of {}", regs.len());
    // SAFETY: every register, place and slot index an operation, edge, call,
    // piece or return names is below its body's count (`lower::verify`, run on
    // each lowered body), and `flat_blocks` asserts its register, place and
    // slot files are those counts long before it runs an operation.
    unsafe { *regs.get_unchecked(i as usize) }
}

/// Write register `i` of `regs`.
#[inline(always)]
fn set<T>(regs: &mut [T], i: u32, x: T) {
    debug_assert!((i as usize) < regs.len(), "register {i} of {}", regs.len());
    // SAFETY: as `get`'s: `lower::verify` proved `i` below the count `flat_blocks`
    // asserted `regs`' length equals.
    unsafe { *regs.get_unchecked_mut(i as usize) = x }
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

/// An operation's stop or the activation's end.
enum Flow {
    Stop(Stop),
    Done(X<()>),
}

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

    /// Charge `n` executed instructions.
    #[inline(always)]
    fn charge(&mut self, n: u32) {
        let n = u64::from(n);
        if self.instructions >= n {
            self.instructions -= n;
        } else {
            self.overdraw(n);
        }
    }

    /// A run of `n` instructions, more than are left: it takes them all and
    /// owes the rest. Nothing is owed when a run starts, since every operation
    /// that ends one settles what its run owes first.
    #[cold]
    #[inline(never)]
    fn overdraw(&mut self, n: u64) {
        assert!(self.owed == 0, "evaluator instruction budget exhausted");
        self.owed = n - self.instructions;
        self.instructions = 0;
    }

    /// Panic when the operation at `pc` is at or past the instruction the tree
    /// walker's exhausted budget stops at: its run owes more than the
    /// instructions after it.
    #[inline(always)]
    fn settled(&self, l: &Lowered, pc: usize) {
        if self.owed != 0 {
            owing(l, pc, self.owed);
        }
    }

    /// `Op::Store` of `x`.
    #[inline(always)]
    fn flat_store(&mut self, places: &[(u64, At)], address: u32, mut x: u64, ty: Ty) {
        if let Some(k) = self.scrambled.as_mut()
            && ty.is_float()
            && float(ty, x).is_nan()
        {
            x = scrambled(ty, *k);
            *k += 1;
        }
        let (p, held) = get(places, address);
        let n = bytes_of(ty);
        self.touch(AddressId(address), p, n, true);
        self.store(p, n, x);
        if let At::Word(w, size) = held {
            self.check_carrier(p - w.offset(), size, Some(w));
        }
    }

    /// `inst`'s `StorageLive` of a slot of `size` bytes at `p`, or with none its
    /// `StorageDead`.
    #[inline(never)]
    fn storage(&mut self, p: u64, size: Option<u64>) {
        let blk = &mut self.mem[split(p).0];
        blk.bytes.clear();
        blk.marks.clear();
        blk.live = size.is_some();
        if let Some(size) = size {
            blk.bytes.resize(size as usize, 0);
        }
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
            Base::Slot(s) => get(slots, s),
            Base::Value(v) => get(vals, v),
            Base::Place(x) => get(places, x).0,
        };
        for &piece in &l.pieces[start as usize..end as usize] {
            match piece {
                Piece::Offset(off) => p += off,
                Piece::Deref { counted: false } => p = self.load(p, 8),
                Piece::Deref { counted: true } => p = self.load(p, 8) + 8,
                Piece::Element { index, len, stride } => {
                    let i = get(vals, index);
                    if i >= len {
                        return Err(Stop::Cause("Bounds"));
                    }
                    p += i * stride;
                }
                Piece::Scaled { index, size } => p += get(vals, index) * size,
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
        let sources = &l.args[c.args.0 as usize..c.args.1 as usize];
        if let Callee::Direct(f) = c.callee
            && !self.checks
            && let Some(r) = self.flat_direct(t, f, sources, vals, places)
        {
            r?;
            return self.flat_results(l, c, vals);
        }
        let mut args = self.take_words();
        args.extend(sources.iter().map(|s| s.of(vals, places)));
        let held = self.frozen.len();
        if self.checks {
            self.lend(&self.m.signatures[c.signature.index()], &args);
        }
        let r = match c.callee {
            Callee::Direct(f) => self.call(t, f, &args),
            Callee::Indirect(v) | Callee::PackExport(v) => {
                self.call(t, FuncId((get(vals, v.0) - FN_BASE) as u32), &args)
            }
            Callee::Runtime(op) => self.rt(t, op, &args).map(|r| {
                self.results.clear();
                self.results.extend_from_slice(&r);
            }),
        };
        self.frozen.truncate(held);
        self.words.push(args);
        r?;
        self.flat_results(l, c, vals)
    }

    /// Call `c`'s results, from `results`, to its registers.
    #[inline(always)]
    fn flat_results(&mut self, l: &Lowered, c: &Call, vals: &mut [u64]) -> X<()> {
        let results = &l.results[c.results.0 as usize..c.results.1 as usize];
        for (&d, &w) in results.iter().zip(&self.results) {
            set(vals, d, w);
        }
        Ok(())
    }

    /// A direct call of lowered `f` when checks are off: `call` and
    /// `activation`, with the arguments read from the caller's registers into
    /// the callee's. None when `f` is not lowered (or this run is observed).
    fn flat_direct(
        &mut self,
        t: &mut Task,
        f: FuncId,
        sources: &[Source],
        vals: &[u64],
        places: &[(u64, At)],
    ) -> Option<X<()>> {
        let m = self.m;
        let b = m.functions[f.index()]
            .body
            .as_ref()
            .expect("external functions do not run here");
        let callee = self.lowered(f, b)?;
        self.enter(t);
        let mut slots = self.take_words();
        self.frame_slots(&mut slots, callee.slot_sizes());
        // FAIL-17: the frame counts toward the allowance as `activation`'s does.
        let frame_bytes = callee.slot_sizes().iter().sum::<u64>();
        crate::TASK_FRAME_BYTES.with(|live| live.set(live.get() + frame_bytes));
        self.enter_frame(t, f);
        let r = self.flat_blocks(t, &callee, b, &slots, Args::Caller(sources, vals, places));
        t.at.pop();
        self.free_slots(slots);
        crate::TASK_FRAME_BYTES.with(|live| live.set(live.get() - frame_bytes));
        Some(r)
    }

    /// The operation at `pc` stopped: its run's rest is given back, the cause
    /// recorded at its site, and the run goes on at its failure edge's block,
    /// whose first operation this returns.
    #[cold]
    #[inline(never)]
    fn flat_stop(
        &mut self,
        t: &mut Task,
        l: &Lowered,
        b: &Body,
        pc: usize,
        stop: Stop,
    ) -> Option<usize> {
        let at = l.ops[pc].at();
        let rest = u64::from(l.rest[pc]);
        assert!(rest >= self.owed, "evaluator instruction budget exhausted");
        self.instructions += rest - self.owed;
        self.owed = 0;
        t.at.last_mut().unwrap().at = Pos::Inst(at);
        self.stopped(t, stop);
        let fb = b.insts[at as usize].fail(b)?;
        let (start, charge) = l.blocks[fb.index()];
        self.charge(charge);
        Some(start as usize)
    }

    /// Block `block`'s terminator when it is not a jump, branch or return: the
    /// first operation of the block it goes on in, or the activation's end.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn flat_exit(
        &mut self,
        t: &mut Task,
        l: &Lowered,
        b: &Body,
        block: u32,
        slots: &[u64],
        vals: &mut [u64],
        places: &[(u64, At)],
    ) -> Result<usize, X<()>> {
        match &b.terminators[block as usize] {
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let x = vals[value.index()];
                let arms = arms.get(&b.pools.arms).unwrap();
                let edge = arms
                    .iter()
                    .find(|a| a.value == x)
                    .map_or(otherwise, |a| &a.edge);
                Ok(self.flat_edge(l, edge.0, vals))
            }
            Terminator::Raise { cause: c, cleanup } => {
                match self.raise(t, b, slots, vals, places, BlockId(block), c, *cleanup) {
                    Ok(c) => {
                        let (start, charge) = l.blocks[c.index()];
                        self.charge(charge);
                        Ok(start as usize)
                    }
                    Err(stop) => Err(Err(stop)),
                }
            }
            Terminator::Propagate => Err(Err(Stop::Chained)),
            Terminator::Unreachable => panic!("an unreachable point was reached"),
            _ => unreachable!("lowered to its own operation"),
        }
    }

    /// Take edge `edge`: its moves, then its target's entry; the target's first
    /// operation.
    #[inline(always)]
    fn flat_edge(&mut self, l: &Lowered, edge: u32, vals: &mut [u64]) -> usize {
        let e = &l.edges[edge as usize];
        let moves = &l.moves[e.moves.0 as usize..e.moves.1 as usize];
        if e.overlap {
            let mut words = self.take_words();
            words.extend(moves.iter().map(|&(_, s)| get(vals, s)));
            for (&(d, _), &w) in moves.iter().zip(&words) {
                set(vals, d, w);
            }
            self.words.push(words);
        } else {
            for &(d, s) in moves {
                set(vals, d, get(vals, s));
            }
        }
        self.charge(e.charge);
        e.start as usize
    }

    /// The tree walker's `blocks`, run over `l`.
    pub(super) fn flat_blocks(
        &mut self,
        t: &mut Task,
        l: &Lowered,
        b: &Body,
        slots: &[u64],
        args: Args,
    ) -> X<()> {
        let depth = t.limits.len();
        let reserved = t.reserved.len();
        let mut vals = self.take_words();
        vals.extend_from_slice(&l.init);
        let mut vectors = simd::Vectors::default();
        let mut places = self.places.pop().unwrap_or_default();
        places.clear();
        places.resize(b.addresses.len(), (0u64, At::Other));
        // What `get` and `set` rely on: the files are the verified counts long.
        assert_eq!(
            l.counts,
            Counts {
                values: vals.len(),
                addresses: places.len(),
                slots: slots.len(),
            },
            "a flat frame differs from its lowered body"
        );
        let mut frame_charges = HashMap::new();
        match args {
            Args::Words(args) => {
                for (&p, &a) in l.params.iter().zip(args) {
                    set(&mut vals, p, a);
                }
            }
            Args::Caller(sources, from, places) => {
                for (&p, &s) in l.params.iter().zip(sources) {
                    set(&mut vals, p, s.of(from, places));
                }
            }
        }
        let site = |t: &mut Task, at: u32| t.at.last_mut().unwrap().at = Pos::Inst(at);
        let (start, charge) = l.blocks[0];
        self.charge(charge);
        let mut pc = start as usize;
        let r = loop {
            let r = match l.ops[pc] {
                Op::Int {
                    result,
                    op,
                    signed,
                    width,
                    left,
                    right,
                    ..
                } => {
                    let (a, c) = (get(&vals, left), get(&vals, right));
                    match int_fast(op, signed, u32::from(width), a, c) {
                        Some(x) => {
                            set(&mut vals, result, x);
                            pc += 1;
                            continue;
                        }
                        None => Flow::Stop(Stop::Cause(arithmetic(op, c))),
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
                    let (a, c) = (get(&vals, left), get(&vals, right));
                    match int_binary(ty, op, a, c) {
                        Some(x) => {
                            set(&mut vals, result, x);
                            pc += 1;
                            continue;
                        }
                        None => Flow::Stop(Stop::Cause(arithmetic(op, c))),
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
                    let x = compare(op, signed, shift, get(&vals, left), get(&vals, right));
                    set(&mut vals, result, u64::from(x));
                    pc += 1;
                    continue;
                }
                Op::Slot { address, slot } => {
                    set(
                        &mut places,
                        address,
                        (get(slots, slot), l.ats[address as usize]),
                    );
                    pc += 1;
                    continue;
                }
                Op::Value { address, value } => {
                    set(
                        &mut places,
                        address,
                        (get(&vals, value), l.ats[address as usize]),
                    );
                    pc += 1;
                    continue;
                }
                Op::Field {
                    address,
                    place,
                    offset,
                } => {
                    let p = get(&places, place).0 + offset;
                    set(&mut places, address, (p, l.ats[address as usize]));
                    pc += 1;
                    continue;
                }
                Op::Address {
                    address, resolved, ..
                } => {
                    self.settled(l, pc);
                    match self.place_of(l, resolved, slots, &vals, &places) {
                        Ok(p) => {
                            set(&mut places, address, (p, l.ats[address as usize]));
                            pc += 1;
                            continue;
                        }
                        Err(stop) => Flow::Stop(stop),
                    }
                }
                Op::Unresolved { address, .. } => {
                    self.settled(l, pc);
                    match self.address(b, slots, &vals, &places, AddressId(address)) {
                        Ok(place) => {
                            set(&mut places, address, place);
                            pc += 1;
                            continue;
                        }
                        Err(stop) => Flow::Stop(stop),
                    }
                }
                Op::Read { result, address, n } => {
                    self.settled(l, pc);
                    let (p, held) = get(&places, address);
                    self.touch(AddressId(address), p, n as usize, false);
                    set(&mut vals, result, self.read_scalar(p, n as usize));
                    if let At::Word(w, size) = held {
                        self.check_carrier(p - w.offset(), size, None);
                    }
                    pc += 1;
                    continue;
                }
                Op::Pointer { result, address } => {
                    set(&mut vals, result, get(&places, address).0);
                    pc += 1;
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
                    let (r, fl) = convert(mode, from, to, get(&vals, operand));
                    set(&mut vals, result, r);
                    if flag != NO_FLAG {
                        set(&mut vals, flag, fl);
                    }
                    pc += 1;
                    continue;
                }
                Op::Store { address, value, ty } => {
                    self.settled(l, pc);
                    self.flat_store(&places, address, get(&vals, value), ty);
                    pc += 1;
                    continue;
                }
                Op::Live { slot, size } => {
                    self.storage(get(slots, slot), Some(size));
                    pc += 1;
                    continue;
                }
                Op::Dead { slot } => {
                    self.storage(get(slots, slot), None);
                    pc += 1;
                    continue;
                }
                Op::Step { at } => {
                    if t.step_bounds.is_empty() {
                        self.steps = self.steps.saturating_add(1);
                        pc += 1;
                        continue;
                    }
                    site(t, at);
                    match self.take_steps(t, 1) {
                        Ok(()) => {
                            pc += 1;
                            continue;
                        }
                        Err(stop) => Flow::Stop(stop),
                    }
                }
                Op::Call { at, call, then } => {
                    self.settled(l, pc);
                    site(t, at);
                    match self.flat_call(t, l, &l.calls[call as usize], &mut vals, &places) {
                        Ok(()) => {
                            self.charge(then);
                            pc += 1;
                            continue;
                        }
                        Err(stop) => Flow::Stop(stop),
                    }
                }
                Op::Generic { at, then } => {
                    self.settled(l, pc);
                    site(t, at);
                    match self.inst(
                        t,
                        b,
                        slots,
                        &mut vals,
                        &mut vectors,
                        &mut places,
                        &mut frame_charges,
                        &b.insts[at as usize],
                    ) {
                        Ok(()) => {
                            self.charge(then);
                            pc += 1;
                            continue;
                        }
                        Err(stop) => Flow::Stop(stop),
                    }
                }
                Op::Jump { edge } => {
                    pc = self.flat_edge(l, edge, &mut vals);
                    continue;
                }
                Op::Branch { condition, yes, no } => {
                    let edge = if get(&vals, condition) != 0 { yes } else { no };
                    pc = self.flat_edge(l, edge, &mut vals);
                    continue;
                }
                Op::CompareBranch {
                    op,
                    signed,
                    shift,
                    left,
                    right,
                    result,
                    yes,
                    no,
                } => {
                    let x = compare(op, signed, shift, get(&vals, left), get(&vals, right));
                    set(&mut vals, result, u64::from(x));
                    pc = self.flat_edge(l, if x { yes } else { no }, &mut vals);
                    continue;
                }
                Op::Return { results } => {
                    self.settled(l, pc);
                    assert_eq!(t.limits.len(), depth, "a return left a bound scope open");
                    assert_eq!(t.reserved.len(), reserved, "a return left an isolate open");
                    self.results.clear();
                    let results = &l.results[results.0 as usize..results.1 as usize];
                    self.results.extend(results.iter().map(|&v| get(&vals, v)));
                    Flow::Done(Ok(()))
                }
                Op::Exit { block } => {
                    self.settled(l, pc);
                    match self.flat_exit(t, l, b, block, slots, &mut vals, &places) {
                        Ok(next) => {
                            pc = next;
                            continue;
                        }
                        Err(done) => Flow::Done(done),
                    }
                }
            };
            match r {
                Flow::Stop(stop) => match self.flat_stop(t, l, b, pc, stop) {
                    Some(next) => pc = next,
                    None => break Err(Stop::Chained),
                },
                Flow::Done(r) => break r,
            }
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

impl Lowered {
    /// By slot, its size: what `place_size` gives for its type.
    pub(super) fn slot_sizes(&self) -> &[u64] {
        &self.sizes
    }
}

/// A failed integer `Binary`'s cause.
fn arithmetic(op: BinaryOp, right: u64) -> &'static str {
    if matches!(op, BinaryOp::Div | BinaryOp::Rem) && right == 0 {
        "Division"
    } else {
        "Overflow"
    }
}

/// `Op::Compare`'s result.
#[inline(always)]
fn compare(op: CompareOp, signed: bool, shift: u8, a: u64, c: u64) -> bool {
    let ord = if signed {
        ((a << shift) as i64).cmp(&((c << shift) as i64))
    } else {
        a.cmp(&c)
    };
    use std::cmp::Ordering::*;
    match op {
        CompareOp::Eq => ord == Equal,
        CompareOp::Ne => ord != Equal,
        CompareOp::Lt => ord == Less,
        CompareOp::Le => ord != Greater,
        CompareOp::Gt => ord == Greater,
        CompareOp::Ge => ord != Less,
    }
}
