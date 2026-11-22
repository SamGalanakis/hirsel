//! Conservative code-word provenance, independent of capture values. A code
//! word is the eight bytes at a known offset of a slot or of a frozen
//! parameter's place: a callable's carrier, or a callable field of a record.
//! Joining every write that touches a word keeps loop-local environments
//! dynamic without guessing a callable after reassignment. A slot any
//! instruction uses in a way not followed here (lent mutably, written at an
//! unknown offset, moved, grown, its pointer stored, passed on an edge or
//! returned) is never bound.
use hirsel_core_ir::*;
use hirsel_core_layout::{FieldTy, Layout, Recipe, Shape, TypeRef};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Origin {
    Function(FuncId),
    /// The code word at this byte offset of the entry parameter's frozen
    /// place, or, at offset zero, the value of an immutable callable
    /// parameter.
    Parameter(u32, u64),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Root {
    Slot(SlotId),
    Parameter(u32),
}
#[derive(Clone, Copy, Debug)]
struct Location {
    root: Root,
    offset: Option<u64>,
}
#[derive(Clone, Copy)]
enum Source {
    Value(ValueId),
    Copy(AddressId),
}
#[derive(Clone, Copy)]
struct Write {
    start: u64,
    len: u64,
    source: Source,
}
#[derive(Clone, Copy)]
enum Def {
    Unknown,
    Const(u64),
    Function(FuncId),
    Pointer(AddressId),
    Read(AddressId),
}

/// Byte offsets of the code words of a value of declared type `t`: each
/// plain `fn`, `own fn` and borrowed field, through inline records.
pub(super) fn code_words(shapes: &[Shape], recipes: &[Recipe], t: TypeRef) -> Vec<u64> {
    fn walk(shapes: &[Shape], recipes: &[Recipe], t: TypeRef, base: u64, out: &mut Vec<u64>) {
        let (Some(fields), Some(Layout::Fixed(layout))) = (
            shapes.get(t.index()).and_then(|s| s.fields(None)),
            recipes.get(t.index()).map(|r| &r.layout),
        ) else {
            return;
        };
        let Some(offsets) = layout.offsets.first() else {
            return;
        };
        for (field, &offset) in fields.iter().zip(offsets) {
            match field.ty {
                FieldTy::Fn | FieldTy::OwnFn | FieldTy::Borrow => out.push(base + offset),
                FieldTy::Inline(r) => walk(shapes, recipes, r, base + offset, out),
                _ => {}
            }
        }
    }
    let mut out = vec![];
    walk(shapes, recipes, t, 0, &mut out);
    out
}

/// Whether a value of declared type `t` holds a code word.
pub(super) fn holds_code(shapes: &[Shape], t: TypeRef) -> bool {
    fn walk(shapes: &[Shape], t: TypeRef, depth: usize) -> bool {
        depth < 64
            && shapes
                .get(t.index())
                .and_then(|s| s.fields(None))
                .is_some_and(|fields| {
                    fields.iter().any(|f| match f.ty {
                        FieldTy::Fn | FieldTy::OwnFn | FieldTy::Borrow => true,
                        FieldTy::Inline(r) => walk(shapes, r, depth + 1),
                        _ => false,
                    })
                })
    }
    walk(shapes, t, 0)
}

/// The entry parameters whose place no write touches during the call, with
/// the place's declared type: callable carriers and viewed records.
pub(super) fn frozen_parameters(
    signature: &Signature,
    body: &Body,
) -> Vec<(ValueId, u32, Option<TypeId>)> {
    let entry = body.blocks[0].params.get(&body.pools.values).unwrap();
    let dest = usize::from(signature.dest.is_some());
    entry
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| {
            let place = i.checked_sub(dest).and_then(|i| signature.params.get(i));
            match place {
                Some(Param::Address {
                    place,
                    loan: Loan::View,
                }) => Some((
                    v,
                    i as u32,
                    match place {
                        PlaceTy::Declared(t) => Some(*t),
                        PlaceTy::Value(_) => None,
                    },
                )),
                _ => None,
            }
        })
        .collect()
}

pub(super) struct Origins<'a> {
    body: &'a Body,
    shapes: &'a [Shape],
    recipes: &'a [Recipe],
    defs: Vec<Def>,
    /// Immutable callable entry values: plain code, or a carrier's pointer.
    callables: BTreeMap<ValueId, u32>,
    /// Entry pointers to frozen places, with the place's declared type.
    frozen: BTreeMap<ValueId, (u32, Option<TypeId>)>,
    addresses: Vec<Option<Location>>,
    writes: BTreeMap<Root, Vec<Write>>,
    escaped: BTreeSet<Root>,
    /// Parameters whose callable or frozen place is stored, copied from or
    /// passed to a call.
    used: BTreeSet<u32>,
    cells: BTreeMap<(Root, u64), Option<Origin>>,
    visiting: BTreeSet<(Root, u64)>,
}
impl<'a> Origins<'a> {
    pub(super) fn new(
        module: &'a Module,
        recipes: &'a [Recipe],
        signature: &Signature,
        body: &'a Body,
    ) -> Self {
        let mut defs = vec![Def::Unknown; body.values.len()];
        for inst in &body.insts {
            match *inst {
                Inst::Const { result, bits } => defs[result.index()] = Def::Const(bits),
                Inst::LayoutConst {
                    result,
                    query: ref query @ (LayoutQuery::HeldSize(_) | LayoutQuery::FieldSize(_)),
                } if recipes.len() == module.types.len() => {
                    defs[result.index()] = Def::Const(query.native(&module.types, recipes))
                }
                Inst::FunctionRef { result, function } => {
                    defs[result.index()] = Def::Function(function)
                }
                Inst::Pointer { result, address } => defs[result.index()] = Def::Pointer(address),
                Inst::Read {
                    result, address, ..
                } => defs[result.index()] = Def::Read(address),
                _ => {}
            }
        }
        let entry = body.blocks[0].params.get(&body.pools.values).unwrap();
        let callables = body
            .callable_parameters
            .iter()
            .map(|v| (*v, entry.iter().position(|p| p == v).unwrap() as u32))
            .collect();
        let frozen = frozen_parameters(signature, body)
            .into_iter()
            .map(|(v, p, t)| (v, (p, t)))
            .collect();
        let mut s = Self {
            body,
            shapes: &module.types,
            recipes,
            defs,
            callables,
            frozen,
            addresses: vec![None; body.addresses.len()],
            writes: BTreeMap::new(),
            escaped: BTreeSet::new(),
            used: BTreeSet::new(),
            cells: BTreeMap::new(),
            visiting: BTreeSet::new(),
        };
        let mut visiting = vec![false; body.addresses.len()];
        for i in 0..body.addresses.len() {
            s.address(AddressId(i as u32), &mut visiting);
        }
        let values = body.values.len();
        for inst in &body.insts {
            // The operands this instruction's own rule accounts for; any other
            // operand naming a slot's place lets the slot escape.
            let mut followed = BTreeSet::new();
            match *inst {
                Inst::Address { .. }
                | Inst::Pointer { .. }
                | Inst::Read { .. }
                | Inst::Case { .. } => {
                    followed.extend(extent::operands(body, inst).0);
                }
                Inst::Store { address, value } => {
                    followed.insert(values + address.index());
                    if let Some(&p) = s.callables.get(&value) {
                        s.used.insert(p);
                    }
                    let len = width(body.values[value.index()]);
                    s.write(address, Some(len), Source::Value(value));
                }
                Inst::Copy { to, from, bytes } => {
                    followed.extend([values + to.index(), values + from.index()]);
                    let len = s.constant(bytes);
                    if len != Some(0) {
                        s.write(to, len, Source::Copy(from));
                    }
                    if let Some(Location {
                        root: Root::Parameter(p),
                        ..
                    }) = s.addresses[from.index()]
                    {
                        s.used.insert(p);
                    }
                }
                Inst::Call { call } => {
                    let c = &body.calls[call.index()];
                    let sig = &module.signatures[c.signature.index()];
                    let dest = usize::from(sig.dest.is_some());
                    for (i, arg) in c.args.get(&body.pools.args).unwrap().iter().enumerate() {
                        let loc = match *arg {
                            Arg::Address(a) => s.addresses[a.index()],
                            Arg::Value(v) => s.pointer(v),
                        };
                        if let Some(Location {
                            root: Root::Parameter(p),
                            ..
                        }) = loc
                        {
                            s.used.insert(p);
                        }
                        if let Arg::Value(v) = *arg
                            && let Some(&p) = s.callables.get(&v)
                        {
                            s.used.insert(p);
                        }
                        let frozen = matches!(
                            i.checked_sub(dest).and_then(|i| sig.params.get(i)),
                            Some(Param::Address {
                                loan: Loan::View | Loan::Take,
                                ..
                            })
                        );
                        if frozen {
                            followed.insert(match *arg {
                                Arg::Address(a) => values + a.index(),
                                Arg::Value(v) => v.index(),
                            });
                        }
                    }
                }
                _ => {}
            }
            for name in extent::operands(body, inst).0 {
                if !followed.contains(&name) {
                    s.escape(name);
                }
            }
        }
        for bi in 0..body.blocks.len() {
            for name in extent::terminator_uses(body, bi) {
                s.escape(name);
            }
        }
        s
    }
    /// The slot a value or address name points into, if any, no longer followed.
    fn escape(&mut self, name: usize) {
        let values = self.body.values.len();
        let loc = if name < values {
            self.pointer(ValueId(name as u32))
        } else {
            self.addresses[name - values]
        };
        if let Some(Location {
            root: root @ Root::Slot(_),
            ..
        }) = loc
        {
            self.escaped.insert(root);
        }
    }
    fn write(&mut self, to: AddressId, len: Option<u64>, source: Source) {
        let Some(loc) = self.addresses[to.index()] else {
            return;
        };
        match (loc.offset, len) {
            (Some(start), Some(len)) => {
                self.writes
                    .entry(loc.root)
                    .or_default()
                    .push(Write { start, len, source })
            }
            _ => {
                self.escaped.insert(loc.root);
            }
        }
    }
    fn constant(&self, v: ValueId) -> Option<u64> {
        match self.defs[v.index()] {
            Def::Const(n) => Some(n),
            _ => None,
        }
    }
    fn pointer(&self, v: ValueId) -> Option<Location> {
        if let Some(&(p, _)) = self.frozen.get(&v) {
            return Some(Location {
                root: Root::Parameter(p),
                offset: Some(0),
            });
        }
        match self.defs[v.index()] {
            Def::Pointer(a) => self.addresses[a.index()],
            _ => None,
        }
    }
    /// The byte offset and type of field `i` of a place of type `ty`.
    fn field(&self, ty: Option<PlaceTy>, i: u32) -> (Option<u64>, Option<PlaceTy>) {
        let Some(PlaceTy::Declared(t)) = ty else {
            return (None, None);
        };
        let offset = match self.recipes.get(t.index()).map(|r| &r.layout) {
            Some(Layout::Fixed(layout)) => layout
                .offsets
                .first()
                .and_then(|row| row.get(i as usize))
                .copied(),
            _ => None,
        };
        let field = self
            .shapes
            .get(t.index())
            .and_then(|s| s.fields(None))
            .and_then(|f| f.get(i as usize));
        let ty = match field.map(|f| &f.ty) {
            Some(FieldTy::Inline(r)) => Some(PlaceTy::Declared(TypeId(r.0))),
            _ => None,
        };
        (offset.filter(|_| field.is_some()), ty)
    }
    fn address(&mut self, a: AddressId, visiting: &mut [bool]) -> Option<Location> {
        if let Some(loc) = self.addresses[a.index()] {
            return Some(loc);
        }
        if visiting[a.index()] {
            return None;
        }
        visiting[a.index()] = true;
        let info = &self.body.addresses[a.index()];
        let base = match info.base {
            AddressBase::Slot(s) => Some((
                Location {
                    root: Root::Slot(s),
                    offset: Some(0),
                },
                Some(self.body.slots[s.index()]),
            )),
            AddressBase::Address(b) => self
                .address(b, visiting)
                .map(|l| (l, Some(self.body.addresses[b.index()].ty))),
            AddressBase::Value(v) => match self.defs[v.index()] {
                Def::Pointer(b) => self
                    .address(b, visiting)
                    .map(|l| (l, Some(self.body.addresses[b.index()].ty))),
                _ => self.frozen.get(&v).map(|&(p, t)| {
                    (
                        Location {
                            root: Root::Parameter(p),
                            offset: Some(0),
                        },
                        t.map(PlaceTy::Declared),
                    )
                }),
            },
        };
        let mut loc = None;
        if let Some((mut l, mut ty)) = base {
            loc = Some(l);
            for step in info.steps.get(&self.body.pools.steps).unwrap() {
                let (offset, next) = match *step {
                    Step::Byte(v) => (self.constant(v), None),
                    Step::Seq(w) => (Some(w.offset()), None),
                    Step::Field(i) => self.field(ty, i),
                    Step::Deref => {
                        loc = None;
                        break;
                    }
                    _ => (None, None),
                };
                l.offset = l.offset.and_then(|n| offset.and_then(|o| n.checked_add(o)));
                ty = next;
                loc = Some(l);
            }
        }
        visiting[a.index()] = false;
        self.addresses[a.index()] = loc;
        loc
    }
    /// Parameter `p`'s callable or frozen place is stored, copied from or
    /// passed to a call.
    pub(super) fn uses(&self, p: u32) -> bool {
        self.used.contains(&p)
    }
    /// The code word at offset `k` of the place argument `a` points to, or
    /// at offset zero the callable `a` is.
    pub(super) fn word(&mut self, a: Arg, k: u64) -> Option<Origin> {
        let loc = match a {
            Arg::Value(v) => {
                if k == 0
                    && let Some(origin) = self.value(v)
                {
                    return Some(origin);
                }
                self.pointer(v)
            }
            Arg::Address(a) => self.addresses[a.index()],
        }?;
        self.cell(loc.root, loc.offset?.checked_add(k)?)
    }
    pub(super) fn value(&mut self, v: ValueId) -> Option<Origin> {
        if let Some(&p) = self.callables.get(&v) {
            return Some(Origin::Parameter(p, 0));
        }
        match self.defs[v.index()] {
            Def::Function(f) => Some(Origin::Function(f)),
            Def::Read(a) => {
                let loc = self.addresses[a.index()]?;
                self.cell(loc.root, loc.offset?)
            }
            _ => None,
        }
    }
    fn cell(&mut self, root: Root, offset: u64) -> Option<Origin> {
        if let Root::Parameter(p) = root {
            return Some(Origin::Parameter(p, offset));
        }
        if self.escaped.contains(&root) {
            return None;
        }
        let key = (root, offset);
        if let Some(origin) = self.cells.get(&key) {
            return *origin;
        }
        if !self.visiting.insert(key) {
            return None;
        }
        let end = offset.checked_add(8);
        let writes = self.writes.get(&root).cloned().unwrap_or_default();
        let mut result = None;
        for w in writes {
            let w_end = w.start.checked_add(w.len);
            if end.is_some_and(|e| e <= w.start) || w_end.is_some_and(|e| e <= offset) {
                continue;
            }
            // A write covering the whole word gives its origin; one covering
            // part of it leaves the word unknown.
            let covers = w.start <= offset && end.zip(w_end).is_some_and(|(e, we)| e <= we);
            let origin = match w.source {
                _ if !covers => None,
                Source::Value(v) if w.start == offset && w.len == 8 => self.value(v),
                Source::Value(_) => None,
                Source::Copy(from) => self.addresses[from.index()].and_then(|l| {
                    let at = l.offset?.checked_add(offset - w.start)?;
                    self.cell(l.root, at)
                }),
            };
            if origin.is_none() || result.is_some_and(|r| Some(r) != origin) {
                result = None;
                break;
            }
            result = origin;
        }
        self.visiting.remove(&key);
        self.cells.insert(key, result);
        result
    }
}

/// The bytes a value of type `t` occupies when stored.
fn width(t: Ty) -> u64 {
    match t {
        Ty::Scalar(s) => s.size(),
        Ty::Ptr => 8,
        Ty::Vector(v) => v.size(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carrier_reassignment_and_mutable_escape_never_bind_the_old_code_word() {
        let text = r#"core v1
sig g0 = () -> ()
sig g1 = (ptr) -> ()
inst n0 = app@1/main/root [] []
inst n1 = app@1/main/a [] []
inst n2 = app@1/main/b [] []
inst n3 = app@1/main/mutate [] []
fn f0 = n0 g0 {
 slot s0 = ptr
 addr a0 = s0 : ptr
b0:
 v0: ptr = fnref f1
 v1: ptr = fnref f2
 store a0, v0
 v2: ptr = read a0
 return
}
fn f1 = n1 g0 extern
fn f2 = n2 g0 extern
fn f3 = n3 g1 extern
"#;
        let origin = |m: &Module| {
            let recipes = hirsel_core_layout::compute(&m.types).unwrap();
            let f = &m.functions[0];
            let b = f.body.as_ref().unwrap();
            Origins::new(m, &recipes, &m.signatures[f.signature.index()], b).value(ValueId(2))
        };
        let mut m = hirsel_core_ir::text::parse(text).unwrap();
        assert_eq!(origin(&m), Some(Origin::Function(FuncId(1))));
        m.functions[0].body.as_mut().unwrap().insts.insert(
            3,
            Inst::Store {
                address: AddressId(0),
                value: ValueId(1),
            },
        );
        assert_ne!(origin(&m), Some(Origin::Function(FuncId(1))));
        m.functions[0].body.as_mut().unwrap().insts.remove(3);
        let b = m.functions[0].body.as_mut().unwrap();
        let v = ValueId(b.values.len() as u32);
        b.values.push(Ty::Ptr);
        b.insts.insert(
            3,
            Inst::Pointer {
                result: v,
                address: AddressId(0),
            },
        );
        let args = PoolRange::push(&mut b.pools.args, [Arg::Value(v)]);
        b.calls.push(CallSite {
            callee: Callee::Direct(FuncId(3)),
            signature: SigId(1),
            args,
            results: PoolRange::EMPTY,
            fail: None,
            cold: false,
        });
        b.insts.insert(4, Inst::Call { call: CallId(0) });
        assert_eq!(origin(&m), None);
    }
}
