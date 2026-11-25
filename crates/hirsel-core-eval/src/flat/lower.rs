//! Lowering a body for the flat engine: its addresses resolved, its integer
//! operations' widths, its calls' registers, its terminators and its runs'
//! instruction charges; and the verifier its register access and its frames'
//! constant template rest on.
use super::*;

/// `int_binary` of integers of `width` bits, on u64 or i64, with its results
/// and refusals: an unsigned operand is its whole word and a signed one its low
/// `width` bits sign-extended, as `int` reads them.
pub(super) fn int_fast(op: BinaryOp, signed: bool, width: u32, a: u64, c: u64) -> Option<u64> {
    let low = |x: u64| {
        if width == 64 {
            x
        } else {
            x & ((1 << width) - 1)
        }
    };
    if signed {
        let shift = 64 - width;
        let x = ((a << shift) as i64) >> shift;
        let y = ((c << shift) as i64) >> shift;
        let fit = |v: Option<i64>| {
            v.filter(|&v| (v << shift) >> shift == v)
                .map(|v| low(v as u64))
        };
        match op {
            BinaryOp::Add => fit(x.checked_add(y)),
            BinaryOp::Sub => fit(x.checked_sub(y)),
            BinaryOp::Mul => fit(x.checked_mul(y)),
            // i64::MIN / -1 does not fit i64 nor any narrower width.
            BinaryOp::Div => fit(x.checked_div(y)),
            // x % -1 is 0 for every x, i64::MIN too.
            BinaryOp::Rem => (y != 0).then(|| low(x.wrapping_rem(y) as u64)),
            BinaryOp::Shl | BinaryOp::Shr if !(0..i64::from(width)).contains(&y) => None,
            BinaryOp::Shl => Some(low(a << y)),
            BinaryOp::Shr => Some(low((x >> y) as u64)),
            BinaryOp::Min => Some(if x <= y { a } else { c }),
            BinaryOp::Max => Some(if x >= y { a } else { c }),
            _ => int_bits(op, width, a, c),
        }
    } else {
        let fit = |v: Option<u64>| v.filter(|&v| v == low(v));
        match op {
            BinaryOp::Add => fit(a.checked_add(c)),
            BinaryOp::Sub => fit(a.checked_sub(c)),
            BinaryOp::Mul => fit(a.checked_mul(c)),
            BinaryOp::Div => fit(a.checked_div(c)),
            BinaryOp::Rem => (c != 0).then(|| low(a % c)),
            BinaryOp::Shl | BinaryOp::Shr if c >= u64::from(width) => None,
            BinaryOp::Shl => Some(low(a << c)),
            BinaryOp::Shr => Some(low(a >> c)),
            BinaryOp::Min => Some(a.min(c)),
            BinaryOp::Max => Some(a.max(c)),
            _ => int_bits(op, width, a, c),
        }
    }
}

/// The integer operations whose result does not read a sign.
fn int_bits(op: BinaryOp, width: u32, a: u64, c: u64) -> Option<u64> {
    let low = |x: u64| {
        if width == 64 {
            x
        } else {
            x & ((1 << width) - 1)
        }
    };
    match op {
        BinaryOp::BitAnd => Some(a & c),
        BinaryOp::BitOr => Some(a | c),
        BinaryOp::BitXor => Some(a ^ c),
        BinaryOp::Rotl => {
            let (x, n) = (low(a), low(c) % u64::from(width));
            Some(low(x << n | x.checked_shr(width - n as u32).unwrap_or(0)))
        }
        _ => unreachable!("float only"),
    }
}

impl Lowered {
    /// Count the runs of the block whose operations start at `start` and end
    /// with the last one lowered, from each operation's instruction count in
    /// `rest`: each operation's `rest`, and each call's and generic
    /// instruction's `then`. What the block's first run charges.
    fn runs(&mut self, start: usize) -> u32 {
        // The instructions after `pc` to the end of its run.
        let mut after = 0;
        for pc in (start..self.ops.len()).rev() {
            if let Op::Call { then, .. } | Op::Generic { then, .. } = &mut self.ops[pc] {
                *then = after;
                after = 0;
            }
            let count = std::mem::replace(&mut self.rest[pc], after);
            after += count;
        }
        after
    }
}

/// Prove every register, place and slot index `l` names below its body's
/// counts, which `flat`'s unchecked register access rests on, and that no
/// register has two definitions among `consts` (the constants' registers), the
/// operations' results and flags, the calls' results and `b`'s block
/// parameters, which the frame's constant template rests on: a constant's
/// register holds its constant from the frame's start, so nothing else may
/// write it. A generic instruction's results are not named here; wf's single
/// definition covers them. A body that fails is a lowering bug.
pub(super) fn verify(l: &Lowered, b: &Body, consts: &[u32]) {
    let Counts {
        values,
        addresses,
        slots,
    } = l.counts;
    let value = |i: u32| assert!((i as usize) < values, "flat: value {i} of {values}");
    let place = |i: u32| assert!((i as usize) < addresses, "flat: place {i} of {addresses}");
    let slot = |i: u32| assert!((i as usize) < slots, "flat: slot {i} of {slots}");
    for op in &l.ops {
        match *op {
            Op::Generic { .. }
            | Op::Call { .. }
            | Op::Step { .. }
            | Op::Jump { .. }
            | Op::Return { .. }
            | Op::Exit { .. } => {}
            Op::Int {
                result,
                left,
                right,
                ..
            }
            | Op::Wide {
                result,
                left,
                right,
                ..
            }
            | Op::Compare {
                result,
                left,
                right,
                ..
            }
            | Op::CompareBranch {
                result,
                left,
                right,
                ..
            } => [result, left, right].into_iter().for_each(value),
            Op::Slot { address, slot: s } => (place(address), slot(s)).1,
            Op::Value { address, value: v } => (place(address), value(v)).1,
            Op::Field {
                address, place: p, ..
            } => (place(address), place(p)).1,
            Op::Address { address, .. } | Op::Unresolved { address, .. } => place(address),
            Op::Read {
                result, address, ..
            }
            | Op::Pointer { result, address } => (value(result), place(address)).1,
            Op::Convert {
                result,
                flag,
                operand,
                ..
            } => {
                value(result);
                value(operand);
                if flag != NO_FLAG {
                    value(flag);
                }
            }
            Op::Store {
                address, value: v, ..
            } => (place(address), value(v)).1,
            Op::Live { slot: s, .. } | Op::Dead { slot: s } => slot(s),
            Op::Branch { condition, .. } => value(condition),
        }
    }
    for &(d, s) in &l.moves {
        value(d);
        value(s);
    }
    for &(base, _) in &l.addresses {
        match base {
            Base::Slot(s) => slot(s),
            Base::Value(v) => value(v),
            Base::Place(x) => place(x),
        }
    }
    for piece in &l.pieces {
        match *piece {
            Piece::Element { index, .. } | Piece::Scaled { index, .. } => value(index),
            Piece::Offset(_) | Piece::Deref { .. } => {}
        }
    }
    for c in &l.calls {
        if let Callee::Indirect(v) = c.callee {
            value(v.0);
        }
    }
    for s in &l.args {
        match *s {
            Source::Value(v) => value(v),
            Source::Place(a) => place(a),
        }
    }
    l.results.iter().chain(&l.params).copied().for_each(value);
    let mut defined = vec![false; values];
    let mut define = |i: u32| {
        value(i);
        assert!(
            !std::mem::replace(&mut defined[i as usize], true),
            "flat: v{i} defined twice"
        );
    };
    consts.iter().copied().for_each(&mut define);
    for op in &l.ops {
        match *op {
            Op::Int { result, .. }
            | Op::Wide { result, .. }
            | Op::Compare { result, .. }
            | Op::CompareBranch { result, .. }
            | Op::Read { result, .. }
            | Op::Pointer { result, .. } => define(result),
            Op::Convert { result, flag, .. } => {
                define(result);
                if flag != NO_FLAG {
                    define(flag);
                }
            }
            _ => {}
        }
    }
    for c in &l.calls {
        l.results[c.results.0 as usize..c.results.1 as usize]
            .iter()
            .copied()
            .for_each(&mut define);
    }
    for blk in &b.blocks {
        let params = blk.params.get(&b.pools.values).unwrap();
        params.iter().for_each(|v| define(v.0));
    }
}

fn add_offset(pieces: &mut Vec<Piece>, off: u64) {
    match pieces.last_mut() {
        Some(Piece::Offset(o)) => *o += off,
        _ => pieces.push(Piece::Offset(off)),
    }
}

impl<'m, H: Host<'m>> Machine<'m, H> {
    pub(super) fn lower(&self, b: &Body) -> Option<Lowered> {
        if b.values.iter().any(|t| matches!(t, Ty::Vector(_))) {
            return None;
        }
        let ty = |v: ValueId| b.values[v.index()];
        let mut l = Lowered {
            ops: Vec::with_capacity(b.insts.len() + b.blocks.len()),
            rest: Vec::with_capacity(b.insts.len() + b.blocks.len()),
            init: vec![0; b.values.len()],
            blocks: Vec::with_capacity(b.blocks.len()),
            edges: Vec::with_capacity(b.edges.len()),
            moves: Vec::new(),
            ats: Vec::new(),
            addresses: Vec::new(),
            pieces: Vec::new(),
            calls: Vec::new(),
            args: Vec::new(),
            results: Vec::new(),
            params: b.blocks[0]
                .params
                .get(&b.pools.values)
                .unwrap()
                .iter()
                .map(|v| v.0)
                .collect(),
            sizes: b.slots.iter().map(|&s| self.place_size(s)).collect(),
            counts: Counts {
                values: b.values.len(),
                addresses: b.addresses.len(),
                slots: b.slots.len(),
            },
        };
        let mut consts = Vec::new();
        let mut resolved = vec![None; b.addresses.len()];
        for a in 0..b.addresses.len() {
            self.resolve(b, a, &mut resolved, &mut l);
        }
        l.ats = resolved
            .iter()
            .map(|r| r.flatten().map_or(At::Other, |(at, _)| at))
            .collect();
        for (k, blk) in b.blocks.iter().enumerate() {
            let start = l.ops.len();
            // The instructions since the last operation that are not one: the
            // next operation counts them.
            let mut pending = 0;
            let first = blk.insts.start;
            for (k, inst) in blk.insts.get(&b.insts).unwrap().iter().enumerate() {
                let at = first + k as u32;
                let op = match *inst {
                    // A builtin's use marks the code it came from; it does not run.
                    Inst::BuiltinUse { .. } => continue,
                    Inst::Call { call } => {
                        let site = &b.calls[call.index()];
                        let args = l.args.len() as u32;
                        let sources = site.args.get(&b.pools.args).unwrap();
                        l.args.extend(sources.iter().map(|a| match *a {
                            Arg::Value(v) => Source::Value(v.0),
                            Arg::Address(a) => Source::Place(a.0),
                        }));
                        let results = l.results.len() as u32;
                        let values = site.results.get(&b.pools.values).unwrap();
                        l.results.extend(values.iter().map(|v| v.0));
                        l.calls.push(Call {
                            callee: site.callee,
                            signature: site.signature,
                            args: (args, l.args.len() as u32),
                            results: (results, l.results.len() as u32),
                        });
                        Op::Call {
                            at,
                            call: l.calls.len() as u32 - 1,
                            then: 0,
                        }
                    }
                    Inst::Step { .. } => Op::Step { at },
                    // A value is defined once (wf), and a constant's register
                    // holds nothing before its definition is read: the frame's
                    // registers start with it.
                    Inst::Const { result, bits } => {
                        l.init[result.index()] = bits;
                        consts.push(result.0);
                        pending += 1;
                        continue;
                    }
                    Inst::Binary {
                        result,
                        op,
                        left,
                        right,
                        ..
                    } if !ty(left).is_float() => match ty(left).int() {
                        Some((signed, width)) => Op::Int {
                            at,
                            result: result.0,
                            op,
                            signed,
                            width: width as u8,
                            left: left.0,
                            right: right.0,
                        },
                        None => Op::Wide {
                            at,
                            result: result.0,
                            op,
                            ty: ty(left),
                            left: left.0,
                            right: right.0,
                        },
                    },
                    Inst::Compare {
                        result,
                        op,
                        left,
                        right,
                    } if !ty(left).is_float() => {
                        let (signed, width) = ty(left).int().unwrap_or((false, 64));
                        Op::Compare {
                            result: result.0,
                            op,
                            signed,
                            shift: (64 - width) as u8,
                            left: left.0,
                            right: right.0,
                        }
                    }
                    Inst::Address { address, .. } => {
                        let address = address.0;
                        match resolved[address as usize].flatten() {
                            None => Op::Unresolved { at, address },
                            Some((_, Resolution::Slot(slot))) => Op::Slot { address, slot },
                            Some((_, Resolution::Value(value))) => Op::Value { address, value },
                            Some((_, Resolution::Field(place, offset))) => Op::Field {
                                address,
                                place,
                                offset,
                            },
                            Some((_, Resolution::Pieces(resolved))) => Op::Address {
                                at,
                                address,
                                resolved,
                            },
                        }
                    }
                    Inst::Read {
                        result, address, ..
                    } => Op::Read {
                        result: result.0,
                        address: address.0,
                        n: bytes_of(ty(result)) as u8,
                    },
                    Inst::Pointer { result, address } => Op::Pointer {
                        result: result.0,
                        address: address.0,
                    },
                    Inst::Convert {
                        result,
                        flag,
                        operand,
                        mode,
                    } => Op::Convert {
                        result: result.0,
                        flag: flag.map_or(NO_FLAG, |f| f.0),
                        operand: operand.0,
                        mode,
                        from: ty(operand),
                        to: ty(result),
                    },
                    Inst::StorageLive { slot } => Op::Live {
                        slot: slot.0,
                        size: self.place_size(b.slots[slot.index()]),
                    },
                    Inst::StorageDead { slot } => Op::Dead { slot: slot.0 },
                    Inst::Store { address, value } => Op::Store {
                        address: address.0,
                        value: value.0,
                        ty: ty(value),
                    },
                    _ => Op::Generic { at, then: 0 },
                };
                l.ops.push(op);
                l.rest.push(1 + pending);
                pending = 0;
            }
            let terminator = self.lower_terminator(b, k, start, pending, &mut l);
            l.ops.push(terminator);
            let charge = 1 + l.runs(start);
            l.blocks.push((start as u32, charge));
        }
        for e in &b.edges {
            let params = b.blocks[e.target.index()]
                .params
                .get(&b.pools.values)
                .unwrap();
            let args = e.args.get(&b.pools.values).unwrap();
            let start = l.moves.len() as u32;
            l.moves
                .extend(params.iter().zip(args).map(|(p, a)| (p.0, a.0)));
            let these = &l.moves[start as usize..];
            let overlap = these
                .iter()
                .any(|&(d, _)| these.iter().any(|&(_, s)| s == d));
            let (target, charge) = l.blocks[e.target.index()];
            l.edges.push(Edge {
                start: target,
                charge,
                moves: (start, l.moves.len() as u32),
                overlap,
            });
        }
        verify(&l, b, &consts);
        Some(l)
    }

    /// Block `k`'s terminator, whose block's operations start at `start`, after
    /// `pending` instructions that are not operations; a branch on the block's
    /// last operation's comparison takes it in. Its own one is the block entry's.
    fn lower_terminator(
        &self,
        b: &Body,
        k: usize,
        start: usize,
        pending: u32,
        l: &mut Lowered,
    ) -> Op {
        l.rest.push(pending);
        match b.terminators[k] {
            Terminator::Jump(e) => Op::Jump { edge: e.0 },
            Terminator::Branch { condition, yes, no } => match l.ops[start..].last() {
                Some(&Op::Compare {
                    result,
                    op,
                    signed,
                    shift,
                    left,
                    right,
                }) if result == condition.0 => {
                    l.ops.pop();
                    let compare = l.rest.pop().unwrap();
                    *l.rest.last_mut().unwrap() += compare;
                    Op::CompareBranch {
                        op,
                        signed,
                        shift,
                        left,
                        right,
                        result,
                        yes: yes.0,
                        no: no.0,
                    }
                }
                _ => Op::Branch {
                    condition: condition.0,
                    yes: yes.0,
                    no: no.0,
                },
            },
            Terminator::Return(r) => {
                let first = l.results.len() as u32;
                let values = r.get(&b.pools.values).unwrap();
                l.results.extend(values.iter().map(|v| v.0));
                Op::Return {
                    results: (first, l.results.len() as u32),
                }
            }
            _ => Op::Exit { block: k as u32 },
        }
    }

    /// Resolve address `a`, and the addresses it is based on, as `address` walks
    /// it: what its place holds and how its pointer is found. None (kept in
    /// `resolved` as `Some(None)`) when a step does not type, where `address`
    /// stops the run.
    #[allow(clippy::type_complexity)]
    fn resolve(
        &self,
        b: &Body,
        a: usize,
        resolved: &mut [Option<Option<(At, Resolution)>>],
        l: &mut Lowered,
    ) -> Option<At> {
        if let Some(r) = resolved[a] {
            return r.map(|(at, _)| at);
        }
        // Until it is done; an address based on itself stays the tree walker's.
        resolved[a] = Some(None);
        let info = &b.addresses[a];
        let typed = || match info.ty {
            PlaceTy::Declared(t) => self.at_of(TypeRef(t.0)),
            PlaceTy::Value(_) => At::Other,
        };
        let (base, mut at) = match info.base {
            AddressBase::Slot(s) => (
                Base::Slot(s.0),
                match b.slots[s.index()] {
                    PlaceTy::Declared(t) => self.at_of(TypeRef(t.0)),
                    PlaceTy::Value(_) => At::Other,
                },
            ),
            AddressBase::Value(v) => (
                Base::Value(v.0),
                match info.ty {
                    PlaceTy::Declared(t) if info.steps.len == 0 => At::Decl(TypeRef(t.0)),
                    _ => At::Other,
                },
            ),
            AddressBase::Address(x) => (Base::Place(x.0), self.resolve(b, x.index(), resolved, l)?),
        };
        let mut pieces = Vec::new();
        for step in info.steps.get(&b.pools.steps).unwrap() {
            match *step {
                Step::Deref => {
                    let counted = match at {
                        At::Ptr(t) => {
                            at = At::Decl(t);
                            false
                        }
                        At::Counted(t) => {
                            at = self.at_of(t);
                            true
                        }
                        _ => return None,
                    };
                    pieces.push(Piece::Deref { counted });
                }
                Step::Field(field) => {
                    let At::Decl(t) = at else { return None };
                    let off;
                    (off, at) = self.field_step_checked(t, None, field)?;
                    add_offset(&mut pieces, off);
                }
                Step::CaseField { case, field } => {
                    let At::Decl(t) = at else { return None };
                    let off;
                    (off, at) = self.field_step_checked(t, Some(case), field)?;
                    add_offset(&mut pieces, off);
                }
                Step::Element(i) => {
                    let At::Array(k) = at else { return None };
                    let (stride, element) = self.array_steps[k];
                    pieces.push(Piece::Element {
                        index: i.0,
                        len: self.arrays[k].1,
                        stride,
                    });
                    at = element;
                }
                Step::Byte(n) => {
                    pieces.push(Piece::Scaled {
                        index: n.0,
                        size: 1,
                    });
                    at = typed();
                }
                Step::Seq(w) => {
                    let At::Seq(size) = at else { return None };
                    add_offset(&mut pieces, w.offset());
                    at = At::Word(w, size);
                }
                Step::Index(i) => {
                    pieces.push(Piece::Scaled {
                        index: i.0,
                        size: self.place_size(info.ty),
                    });
                    at = typed();
                }
            }
        }
        let resolution = match (base, pieces.as_slice()) {
            (Base::Slot(s), []) => Resolution::Slot(s),
            (Base::Value(v), []) => Resolution::Value(v),
            (Base::Place(x), []) => Resolution::Field(x, 0),
            (Base::Place(x), &[Piece::Offset(off)]) => Resolution::Field(x, off),
            _ => {
                let start = l.pieces.len() as u32;
                l.pieces.extend(pieces);
                l.addresses.push((base, (start, l.pieces.len() as u32)));
                Resolution::Pieces(l.addresses.len() as u32 - 1)
            }
        };
        resolved[a] = Some(Some((at, resolution)));
        Some(at)
    }
}
