//! Storage roles (crate docs, "Roles"): derived from a body, never stored.

use crate::{
    AddressBase, AddressId, AllocPlace, Arg, Body, Function, Inst, Loan, Module, Param, PlaceTy,
    SlotId, Step, Terminator, TypeId, ValueId,
};
use hirsel_core_layout::{Body as Shaped, FieldTy, POINTER, Repr, SEQ_CARRIER, Ty};

/// The place a carrier role names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Root {
    Slot(SlotId),
    /// The place of the signature's `i`th parameter (the destination not counted).
    Param(u32),
}

/// What an address's bytes are, when the form says. Two accesses (`read`, `store`,
/// either side of a `copy`) whose addresses have different roles touch disjoint
/// bytes, throughout one activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// The carrier `{data, length, capacity}` held in this slot or lent by this
    /// `mut` or `take` parameter.
    Carrier(Root),
    /// Storage the data word of this sealed parameter's carrier points at.
    Buffer(Root),
    /// Storage any other carrier role's data word points at: one role for all.
    Buffers,
}

/// A carrier type: an own record whose one field is a `seq` of a scalar (`String`,
/// `Bytes`, `List` of a scalar). Its buffer holds scalars only, never a carrier.
pub fn carrier(m: &Module, t: TypeId) -> bool {
    let Some(shape) = m.types.get(t.index()) else {
        return false;
    };
    matches!(
        (&shape.repr, &shape.body),
        (Repr::Own, Shaped::Record(fields))
            if matches!(&fields[..], [f] if matches!(&f.ty, FieldTy::Seq(e) if matches!(**e, FieldTy::Scalar(_))))
    )
}

#[derive(Clone, Copy)]
enum Loc {
    /// This many bytes into a carrier.
    Carrier(Root, u64),
    /// Storage reached through this root's data word.
    Buffer(Root),
}

#[derive(Clone, Copy)]
enum Def {
    Other,
    Root(Root),
    Const(u64),
    Pointer(AddressId),
    /// A pointer read from this address.
    Read(AddressId),
    /// A block this body allocated.
    Fresh,
}

#[derive(Clone, Copy)]
enum Memo {
    Todo,
    Busy,
    Done(Option<Loc>),
}

struct Derive<'a> {
    m: &'a Module,
    b: &'a Body,
    defs: Vec<Def>,
    memo: Vec<Memo>,
}

/// Each address's role, by `AddressId`; None makes no claim.
pub fn roles(m: &Module, f: &Function) -> Vec<Option<Role>> {
    let Some(b) = &f.body else {
        return Vec::new();
    };
    let sig = &m.signatures[f.signature.index()];
    let mut defs = vec![Def::Other; b.values.len()];
    for inst in &b.insts {
        match *inst {
            Inst::Const { result, bits } => defs[result.index()] = Def::Const(bits),
            Inst::Alloc { result, .. } => defs[result.index()] = Def::Fresh,
            Inst::Pointer { result, address } => defs[result.index()] = Def::Pointer(address),
            Inst::Read {
                result, address, ..
            } if b.values[result.index()] == Ty::Ptr => defs[result.index()] = Def::Read(address),
            _ => {}
        }
    }
    // An entry value is the parameter's pointer only while no edge rebinds it.
    // (A failure or cleanup block takes no arguments, so only an edge can.)
    let reentered = b.edges.iter().any(|e| e.target.index() == 0);
    if !reentered && let Some(entry) = b.blocks.first() {
        let values = entry.params.get(&b.pools.values).unwrap_or(&[]);
        let skip = usize::from(sig.dest.is_some());
        for (i, (p, v)) in sig.params.iter().zip(values.iter().skip(skip)).enumerate() {
            if let Param::Address {
                place: PlaceTy::Declared(ty),
                loan: Loan::Mut | Loan::Take,
            } = *p
                && carrier(m, ty)
            {
                defs[v.index()] = Def::Root(Root::Param(i as u32));
            }
        }
    }
    let mut d = Derive {
        m,
        b,
        defs,
        memo: vec![Memo::Todo; b.addresses.len()],
    };
    let locs: Vec<Option<Loc>> = (0..b.addresses.len())
        .map(|a| d.loc(AddressId(a as u32)))
        .collect();
    let sealed = Seal::new(m, b, &d.defs, &locs).sealed(sig.params.len());
    locs.iter()
        .map(|l| {
            l.map(|l| match l {
                Loc::Carrier(root, _) => Role::Carrier(root),
                Loc::Buffer(Root::Param(i)) if sealed[i as usize] => Role::Buffer(Root::Param(i)),
                Loc::Buffer(_) => Role::Buffers,
            })
        })
        .collect()
}

impl Derive<'_> {
    fn loc(&mut self, a: AddressId) -> Option<Loc> {
        match self.memo[a.index()] {
            Memo::Done(l) => return l,
            Memo::Busy => return None,
            Memo::Todo => {}
        }
        self.memo[a.index()] = Memo::Busy;
        let l = self.compute(a);
        self.memo[a.index()] = Memo::Done(l);
        l
    }

    fn compute(&mut self, a: AddressId) -> Option<Loc> {
        let info = &self.b.addresses[a.index()];
        let mut l = match info.base {
            AddressBase::Slot(s) => match self.b.slots[s.index()] {
                PlaceTy::Declared(t) if carrier(self.m, t) => Loc::Carrier(Root::Slot(s), 0),
                _ => return None,
            },
            AddressBase::Address(x) => self.loc(x)?,
            AddressBase::Value(v) => self.value(v)?,
        };
        for step in info.steps.get(&self.b.pools.steps)? {
            l = match (l, *step) {
                // The carrier's one field, its `seq`, starts at its first byte.
                (Loc::Carrier(r, o), Step::Field(0)) => Loc::Carrier(r, o),
                (Loc::Carrier(r, o), Step::Byte(n)) => {
                    Loc::Carrier(r, o.checked_add(self.constant(n)?)?)
                }
                (Loc::Buffer(r), Step::Index(_) | Step::Byte(_)) => Loc::Buffer(r),
                _ => return None,
            };
        }
        match l {
            Loc::Carrier(_, o) => {
                let n = match info.ty {
                    PlaceTy::Value(Ty::Scalar(s)) => s.size(),
                    PlaceTy::Value(Ty::Ptr) => POINTER,
                    PlaceTy::Declared(t) if carrier(self.m, t) => SEQ_CARRIER,
                    PlaceTy::Declared(_) => return None,
                };
                (o.checked_add(n)? <= SEQ_CARRIER).then_some(l)
            }
            Loc::Buffer(_) => Some(l),
        }
    }

    fn value(&mut self, v: ValueId) -> Option<Loc> {
        match self.defs[v.index()] {
            Def::Root(r) => Some(Loc::Carrier(r, 0)),
            Def::Pointer(x) => self.loc(x),
            // The data word of a carrier: its first.
            Def::Read(x) => match self.loc(x)? {
                Loc::Carrier(r, 0) => Some(Loc::Buffer(r)),
                _ => None,
            },
            Def::Other | Def::Const(_) | Def::Fresh => None,
        }
    }

    fn constant(&self, v: ValueId) -> Option<u64> {
        match self.defs[v.index()] {
            Def::Const(c) => Some(c),
            _ => None,
        }
    }
}

/// How a body uses a value that points into a carrier or at its buffer, other than
/// as an address's base, which never moves it.
#[derive(Clone, Copy, PartialEq)]
enum Use {
    /// A `free` operand or an `alloc … like` operand.
    Block,
    /// A comparison.
    Compare,
    /// Anything else: the pointer may reach another carrier or a callee.
    Other,
}

/// Which `mut` and `take` parameter carriers are sealed (crate docs, "Roles"): no
/// buffer reached through one is ever reached through another carrier of the
/// activation, because none leaves the carrier or enters it from elsewhere.
struct Seal<'a> {
    m: &'a Module,
    b: &'a Body,
    defs: &'a [Def],
    locs: &'a [Option<Loc>],
    /// Each address's root, by whether it is the carrier's (or a buffer's) bytes,
    /// however its type and steps read; `None` reaches neither.
    reach: Vec<Option<(Root, bool)>>,
    /// The root a fresh block is installed in, once one store puts it in a data word.
    installed: Vec<Option<Root>>,
    open: Vec<bool>,
}

impl<'a> Seal<'a> {
    fn new(m: &'a Module, b: &'a Body, defs: &'a [Def], locs: &'a [Option<Loc>]) -> Self {
        let mut s = Seal {
            m,
            b,
            defs,
            locs,
            reach: Vec::new(),
            installed: vec![None; b.values.len()],
            open: Vec::new(),
        };
        // An address's base comes before it, so one pass in order sees every base.
        for a in 0..b.addresses.len() {
            let r = match b.addresses[a].base {
                AddressBase::Slot(_) => None,
                AddressBase::Address(x) => s.reach.get(x.index()).copied().flatten(),
                AddressBase::Value(v) => s.value(v),
            };
            s.reach.push(r);
        }
        s
    }

    /// The root a pointer value reaches, and whether it is the root's buffer.
    fn value(&self, v: ValueId) -> Option<(Root, bool)> {
        let at = |x: AddressId| self.reach.get(x.index()).copied().flatten();
        match self.defs[v.index()] {
            Def::Root(r) => Some((r, false)),
            Def::Pointer(x) => at(x),
            Def::Read(x) => match (at(x), self.locs[x.index()]) {
                (Some((r, false)), Some(Loc::Carrier(_, 0))) => Some((r, true)),
                // Any other pointer in a carrier is followed as the carrier itself.
                (Some((r, false)), _) => Some((r, false)),
                // A buffer holds scalars, never a carrier's pointer.
                (Some((_, true)) | None, _) => None,
            },
            Def::Fresh => self.installed[v.index()].map(|r| (r, true)),
            Def::Other | Def::Const(_) => None,
        }
    }

    fn unseal(&mut self, r: Root) {
        if let Root::Param(i) = r
            && let Some(o) = self.open.get_mut(i as usize)
        {
            *o = true;
        }
    }

    fn sealed(mut self, params: usize) -> Vec<bool> {
        self.open = vec![false; params];
        let b = self.b;
        // Installs first: a fresh block stored into one data word joins that root.
        for inst in &b.insts {
            if let Inst::Store { address, value } = *inst
                && let Some(Loc::Carrier(r, 0)) = self.locs[address.index()]
            {
                match (self.defs[value.index()], self.installed[value.index()]) {
                    (Def::Fresh, None) => self.installed[value.index()] = Some(r),
                    (Def::Fresh, Some(r2)) if r2 == r => {}
                    (Def::Fresh, Some(r2)) => {
                        self.unseal(r);
                        self.unseal(r2);
                    }
                    _ => self.unseal(r),
                }
            }
        }
        // An address that reaches a carrier without naming its bytes as a carrier role
        // does says nothing the derivation can follow.
        for a in 0..b.addresses.len() {
            if let Some((r, false)) = self.reach[a]
                && !matches!(self.locs[a], Some(Loc::Carrier(..)))
            {
                self.unseal(r);
            }
        }
        for inst in &b.insts {
            self.inst(inst);
        }
        for t in &b.terminators {
            match t {
                Terminator::Branch { condition: v, .. } | Terminator::Switch { value: v, .. } => {
                    self.used(*v, Use::Other)
                }
                Terminator::Return(vs) => {
                    for v in vs.get(&b.pools.values).unwrap_or(&[]) {
                        self.used(*v, Use::Other);
                    }
                }
                Terminator::Jump(_)
                | Terminator::Unreachable
                | Terminator::Raise { .. }
                | Terminator::Propagate => {}
            }
        }
        for e in &b.edges {
            for v in e.args.get(&b.pools.values).unwrap_or(&[]) {
                self.used(*v, Use::Other);
            }
        }
        self.open.iter().map(|o| !o).collect()
    }

    fn used(&mut self, v: ValueId, how: Use) {
        if let Some((r, buffer)) = self.value(v)
            && !(how == Use::Compare || (how == Use::Block && buffer))
        {
            self.unseal(r);
        }
    }

    /// A carrier's bytes used other than by a word access: copied, lent or dropped.
    fn whole(&mut self, a: AddressId) {
        if let Some((r, false)) = self.reach[a.index()] {
            self.unseal(r);
        }
    }

    fn inst(&mut self, inst: &Inst) {
        let b = self.b;
        match *inst {
            Inst::Store { address, value } => {
                // An install was settled above; a block stored as an install stays in
                // its root.
                match (self.locs[address.index()], self.installed[value.index()]) {
                    (Some(Loc::Carrier(r, 0)), Some(r2)) if r == r2 => {}
                    _ => self.used(value, Use::Other),
                }
            }
            Inst::Compare { left, right, .. } => {
                self.used(left, Use::Compare);
                self.used(right, Use::Compare);
            }
            Inst::Free { block, charge } => {
                self.used(block, Use::Block);
                self.used(charge, Use::Other);
            }
            Inst::Alloc {
                region_entry,
                place,
                charge,
                size,
                ..
            } => {
                if let Some(v) = region_entry {
                    self.used(v, Use::Other);
                }
                match place {
                    AllocPlace::Like(v) => self.used(v, Use::Block),
                    AllocPlace::Region(v) => self.used(v, Use::Other),
                    AllocPlace::Current => {}
                }
                self.used(charge, Use::Other);
                self.used(size, Use::Other);
            }
            Inst::Copy { to, from, bytes } => {
                self.whole(to);
                self.whole(from);
                self.used(bytes, Use::Other);
            }
            Inst::Call { call } => {
                let site = &b.calls[call.index()];
                let sig = &self.m.signatures[site.signature.index()];
                if let crate::Callee::Indirect(v) = site.callee {
                    self.used(v, Use::Other);
                }
                let skip = usize::from(sig.dest.is_some());
                for (i, arg) in site
                    .args
                    .get(&b.pools.args)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    match *arg {
                        Arg::Value(v) => self.used(v, Use::Other),
                        Arg::Address(a) => {
                            let view = i >= skip
                                && matches!(
                                    sig.params.get(i - skip),
                                    Some(Param::Address {
                                        loan: Loan::View,
                                        ..
                                    })
                                );
                            if !view {
                                self.whole(a);
                            }
                        }
                    }
                }
            }
            Inst::Drop { address, .. }
            | Inst::Case { address, .. }
            | Inst::SetCase { address, .. } => self.whole(address),
            Inst::Mark { address, count, .. } | Inst::Unmark { address, count, .. } => {
                self.whole(address);
                self.used(count, Use::Other);
            }
            Inst::Unary { operand, .. }
            | Inst::Convert { operand, .. }
            | Inst::ScalarUnary { operand, .. } => self.used(operand, Use::Other),
            Inst::Binary { left, right, .. } | Inst::IntArith { left, right, .. } => {
                self.used(left, Use::Other);
                self.used(right, Use::Other);
            }
            Inst::MulAdd {
                left,
                right,
                addend,
                ..
            } => {
                self.used(left, Use::Other);
                self.used(right, Use::Other);
                self.used(addend, Use::Other);
            }
            Inst::UpdateMask { mask, .. } => self.used(mask, Use::Other),
            Inst::RegionClose { region } => self.used(region, Use::Other),
            Inst::Trim {
                block,
                region,
                entry,
                previous,
                charge,
            } => {
                for v in [block, region, entry, previous, charge] {
                    self.used(v, Use::Other);
                }
            }
            Inst::LimitEnter { bytes, .. } => self.used(bytes, Use::Other),
            Inst::Share { node, .. }
            | Inst::Release { node, .. }
            | Inst::TryUnwrap { node, .. } => self.used(node, Use::Other),
            Inst::Const { .. }
            | Inst::ReadStatic { .. }
            | Inst::FunctionRef { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::Pointer { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::RegionOpen { .. }
            | Inst::LimitExit => {}
        }
    }
}
