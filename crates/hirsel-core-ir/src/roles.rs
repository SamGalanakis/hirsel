//! Storage roles (crate docs, "Roles"): derived from a body, never stored.

use crate::{
    AddressBase, AddressId, Body, Function, Inst, Loan, Module, Param, PlaceTy, SlotId, Step,
    TypeId, ValueId,
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
    /// Storage a carrier role's data word points at: every carrier's, one role.
    Buffer,
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
    Buffer,
}

#[derive(Clone, Copy)]
enum Def {
    Other,
    Root(Root),
    Const(u64),
    Pointer(AddressId),
    /// A pointer read from this address.
    Read(AddressId),
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
                ty,
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
    (0..b.addresses.len())
        .map(|a| {
            d.loc(AddressId(a as u32)).map(|l| match l {
                Loc::Carrier(root, _) => Role::Carrier(root),
                Loc::Buffer => Role::Buffer,
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
                (Loc::Buffer, Step::Index(_) | Step::Byte(_)) => Loc::Buffer,
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
            Loc::Buffer => Some(l),
        }
    }

    fn value(&mut self, v: ValueId) -> Option<Loc> {
        match self.defs[v.index()] {
            Def::Root(r) => Some(Loc::Carrier(r, 0)),
            Def::Pointer(x) => self.loc(x),
            // The data word of a carrier: its first.
            Def::Read(x) => match self.loc(x)? {
                Loc::Carrier(_, 0) => Some(Loc::Buffer),
                _ => None,
            },
            Def::Other | Def::Const(_) => None,
        }
    }

    fn constant(&self, v: ValueId) -> Option<u64> {
        match self.defs[v.index()] {
            Def::Const(c) => Some(c),
            _ => None,
        }
    }
}
