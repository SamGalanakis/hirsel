//! Specialize the typed calling convention together with a stored body.
use super::*;

/// Address parameters with value semantics can follow their substituted row's
/// carriage. Mutable parameters keep their address, and at a scalar-carried row
/// lend a value place of that scalar, as a direct signature's do (#88).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SignatureAbi {
    pub params: Vec<bool>,
    pub result: bool,
}

/// The place a `mut` parameter at row `id` lends (#88): a value place of the
/// scalar a scalar-carried row is carried as, else the row.
pub(super) fn mut_place(types: &[hirsel_core_layout::Shape], id: TypeId) -> PlaceTy {
    match types[id.index()].carried {
        Carried::Value(t) => PlaceTy::Value(t),
        Carried::Nothing | Carried::Address => PlaceTy::Declared(id),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Change {
    Keep,
    Value(Ty, TypeId),
    /// A `mut` parameter whose row is carried as `Ty`: still lent by address, as
    /// a value place of it.
    Place(Ty),
    Nothing,
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub params: Vec<Change>,
    pub result: Change,
}

impl Plan {
    pub(super) fn signature(module: &Module, sig: &mut Signature, abi: &SignatureAbi) -> Self {
        let mut params = Vec::with_capacity(sig.params.len());
        sig.params = sig
            .params
            .iter()
            .enumerate()
            .filter_map(|(i, &param)| {
                let change = match param {
                    Param::Address {
                        place: PlaceTy::Declared(id),
                        ..
                    } if abi.params.get(i) == Some(&true) => {
                        match module.types[id.index()].carried {
                            Carried::Value(t) => Change::Value(t, id),
                            Carried::Nothing => Change::Nothing,
                            Carried::Address => Change::Keep,
                        }
                    }
                    Param::Address {
                        place: PlaceTy::Declared(id),
                        loan: Loan::Mut,
                    } => match mut_place(&module.types, id) {
                        PlaceTy::Value(t) => Change::Place(t),
                        PlaceTy::Declared(_) => Change::Keep,
                    },
                    _ => Change::Keep,
                };
                params.push(change);
                match change {
                    Change::Keep => Some(param),
                    Change::Value(t, _) => Some(Param::Value(t)),
                    Change::Place(t) => Some(Param::Address {
                        place: PlaceTy::Value(t),
                        loan: Loan::Mut,
                    }),
                    Change::Nothing => None,
                }
            })
            .collect();
        let result = match sig.dest.filter(|_| abi.result) {
            Some(id) => match module.types[id.index()].carried {
                Carried::Value(t) => {
                    sig.dest = None;
                    sig.results = vec![t];
                    Change::Value(t, id)
                }
                Carried::Nothing => {
                    sig.dest = None;
                    Change::Nothing
                }
                Carried::Address => Change::Keep,
            },
            None => Change::Keep,
        };
        Self { params, result }
    }
}

struct Code<'a> {
    body: &'a mut Body,
    insts: Vec<Inst>,
    anchors: Vec<Option<Anchor>>,
    anchor: Option<Anchor>,
}
impl Code<'_> {
    fn emit(&mut self, inst: Inst) {
        self.insts.push(inst);
        self.anchors.push(self.anchor);
    }
    fn value(&mut self, ty: Ty) -> ValueId {
        let id = ValueId(self.body.values.len() as u32);
        self.body.values.push(ty);
        id
    }
    fn address(&mut self, base: AddressBase, steps: &[Step], ty: PlaceTy) -> AddressId {
        let address = AddressId(self.body.addresses.len() as u32);
        let steps = PoolRange::push(&mut self.body.pools.steps, steps.iter().copied());
        self.body.addresses.push(AddressInfo { base, steps, ty });
        self.emit(Inst::Address {
            address,
            fail: None,
        });
        address
    }
    fn field(&mut self, address: AddressId, ty: Ty) -> AddressId {
        if self.body.addresses[address.index()].ty == PlaceTy::Value(ty) {
            address
        } else {
            self.address(AddressBase::Address(address), &[], PlaceTy::Value(ty))
        }
    }
    fn load(&mut self, address: AddressId, ty: Ty) -> ValueId {
        let address = self.field(address, ty);
        let result = self.value(ty);
        self.emit(Inst::Read {
            result,
            address,
            mode: ReadMode::Bits,
        });
        result
    }
    fn store(&mut self, address: AddressId, value: ValueId) {
        let address = self.field(address, self.body.values[value.index()]);
        self.emit(Inst::Store { address, value });
    }
    fn local(&mut self, row: TypeId, pointer: ValueId) -> AddressId {
        let slot = SlotId(self.body.slots.len() as u32);
        self.body.slots.push(PlaceTy::Declared(row));
        self.emit(Inst::StorageLive { slot });
        let address = self.address(AddressBase::Slot(slot), &[], PlaceTy::Declared(row));
        self.emit(Inst::Pointer {
            result: pointer,
            address,
        });
        address
    }
}

pub(super) fn rewrite(
    module: &Module,
    recipes: &[Recipe],
    body: &mut Body,
    old_signature: SigId,
    plans: &HashMap<SigId, Plan>,
    old_calls: &[SigId],
    numeric_sites: &[NumericSite],
) {
    let unchanged =
        !plans[&old_signature].changed() && old_calls.iter().all(|s| !plans[s].changed());
    let dest = convert(module, body, old_signature, plans, old_calls);
    numeric::inline(module, body, numeric_sites);
    key_glue::inline_ordered(module, body);
    // A carrier-only body, such as Map's index installation, has no rigid scalar
    // places to rewrite. Substituting its rows/constants is complete.
    let scalar_place = |p| matches!(p, PlaceTy::Declared(_)) && place_scalar(module, p).is_some();
    let needs_places = body.slots.iter().any(|&p| scalar_place(p))
        || body.insts.iter().any(|inst| match inst {
            Inst::Drop { ty, .. } => recipes[ty.index()].drop.is_none(),
            Inst::Copy { to, from, .. } => {
                scalar_place(body.addresses[to.index()].ty)
                    || scalar_place(body.addresses[from.index()].ty)
            }
            Inst::Pointer { address, .. } => scalar_place(body.addresses[address.index()].ty),
            _ => false,
        });
    if unchanged && !needs_places {
        remap::calls(body);
        return;
    }
    promote(module, recipes, body, dest);
}

impl Plan {
    fn changed(&self) -> bool {
        !matches!(self.result, Change::Keep)
            || self.params.iter().any(|p| !matches!(p, Change::Keep))
    }
    fn keep(sig: &Signature) -> Self {
        Self {
            params: vec![Change::Keep; sig.params.len()],
            result: Change::Keep,
        }
    }
}

/// Concrete signatures follow their rows' carriage at the boundary, exactly as an
/// instance's do (FORM #78): each signature with a recorded ABI is converted by its
/// [`Plan`]; each function row takes the converted signature; each body spills its
/// new value parameters into slots at entry, returns its result from the dest slot
/// and converts its call sites. The body's own representation is unchanged. `skip`
/// holds generic template rows, which an instance converts when it is filled, and a
/// signature naming a symbolic row (`rigid`) is left whole for the instance too; a
/// row already sealed has no recorded ABI, so sealing twice changes nothing.
pub(super) fn seal<T>(
    module: &mut Module,
    abis: &std::collections::BTreeMap<SigId, SignatureAbi>,
    skip: &std::collections::BTreeSet<FuncId>,
    rigid: &std::collections::BTreeMap<TypeId, T>,
) {
    crate::cost_scope!("phase", "body seal");
    let mut plans: HashMap<SigId, Plan> = HashMap::default();
    let mut sealed = HashMap::default();
    let by_address = |t: TypeId| matches!(module.types[t.index()].carried, Carried::Address);
    for (&id, abi) in abis {
        let sig = &module.signatures[id.index()];
        let moves = |(i, p): (usize, &Param)| match p {
            Param::Address {
                place: PlaceTy::Declared(ty),
                ..
            } => abi.params.get(i) == Some(&true) && !by_address(*ty),
            Param::Address { .. } | Param::Value(_) => false,
        };
        if !sig.params.iter().enumerate().any(moves)
            && !sig.dest.is_some_and(|t| abi.result && !by_address(t))
        {
            continue;
        }
        let mut rows = sig.params.iter().filter_map(|p| match p {
            Param::Address {
                place: PlaceTy::Declared(ty),
                ..
            } => Some(*ty),
            Param::Address { .. } | Param::Value(_) => None,
        });
        if rows.any(|t| rigid.contains_key(&t)) || sig.dest.is_some_and(|t| rigid.contains_key(&t))
        {
            continue;
        }
        let mut sig = sig.clone();
        let plan = Plan::signature(module, &mut sig, abi);
        if plan.changed() {
            plans.insert(id, plan);
            sealed.insert(id, sig);
        }
    }
    if plans.is_empty() {
        return;
    }
    let renamed: HashMap<SigId, SigId> = sealed
        .into_iter()
        .map(|(old, sig)| {
            let id = match module.signatures.iter().position(|s| *s == sig) {
                Some(i) => SigId(i as u32),
                None => {
                    module.signatures.push(sig);
                    SigId(module.signatures.len() as u32 - 1)
                }
            };
            (old, id)
        })
        .collect();
    for f in 0..module.functions.len() {
        if skip.contains(&FuncId(f as u32)) {
            continue;
        }
        let own = module.functions[f].signature;
        if let Some(mut body) = module.functions[f].body.take() {
            let old_calls = body.calls.iter().map(|c| c.signature).collect::<Vec<_>>();
            if plans.contains_key(&own) || old_calls.iter().any(|s| plans.contains_key(s)) {
                // This body's signatures: the sealed ones by their plans, the rest kept.
                let local = old_calls
                    .iter()
                    .chain([&own])
                    .map(|&id| {
                        let plan = plans.get(&id).cloned();
                        (
                            id,
                            plan.unwrap_or_else(|| Plan::keep(&module.signatures[id.index()])),
                        )
                    })
                    .collect();
                convert(module, &mut body, own, &local, &old_calls);
                for call in &mut body.calls {
                    if let Some(&id) = renamed.get(&call.signature) {
                        call.signature = id;
                    }
                }
            }
            module.functions[f].body = Some(body);
        }
        if let Some(&id) = renamed.get(&own) {
            module.functions[f].signature = id;
        }
    }
}

/// Convert `body`'s entry, returns and call sites from the signatures it was emitted
/// against to their plans; the dest slot a value result is returned from, if any.
fn convert(
    module: &Module,
    body: &mut Body,
    old_signature: SigId,
    plans: &HashMap<SigId, Plan>,
    old_calls: &[SigId],
) -> Option<SlotId> {
    let own = &plans[&old_signature];
    if !own.changed() && old_calls.iter().all(|s| !plans[s].changed()) {
        return None;
    }
    let old = std::mem::take(&mut body.insts);
    let old_anchors = std::mem::take(&mut body.inst_anchors);
    let entry = body.blocks[0]
        .params
        .get(&body.pools.values)
        .unwrap()
        .to_vec();
    let original = &module.signatures[old_signature.index()];
    let mut c = Code {
        body,
        insts: Vec::with_capacity(old.len()),
        anchors: Vec::with_capacity(old.len()),
        anchor: None,
    };
    let mut params = Vec::with_capacity(entry.len());
    let mut index = usize::from(original.dest.is_some());
    let scalar_dest = match own.result {
        Change::Place(_) => unreachable!("a result is never lent"),
        Change::Keep => {
            if original.dest.is_some() {
                params.push(entry[0]);
            }
            None
        }
        Change::Nothing => {
            c.emit(Inst::Const {
                result: entry[0],
                bits: 0,
            });
            None
        }
        Change::Value(t, row) => Some((c.local(row, entry[0]), t)),
    };
    for change in &own.params {
        let old_value = entry[index];
        index += 1;
        match *change {
            Change::Keep | Change::Place(_) => params.push(old_value),
            Change::Nothing => c.emit(Inst::Const {
                result: old_value,
                bits: 0,
            }),
            Change::Value(t, row) => {
                let value = c.value(t);
                params.push(value);
                let at = c.local(row, old_value);
                c.store(at, value);
            }
        }
    }
    c.body.blocks[0].params = PoolRange::push(&mut c.body.pools.values, params);
    for bi in 0..c.body.blocks.len() {
        let range = c.body.blocks[bi].insts;
        let start = if bi == 0 { 0 } else { c.insts.len() as u32 };
        for i in range.start as usize..(range.start + range.len) as usize {
            c.anchor = old_anchors[i];
            if let Inst::Call { call } = old[i] {
                let plan = &plans[&old_calls[call.index()]];
                let site = c.body.calls[call.index()].clone();
                let args = site.args.get(&c.body.pools.args).unwrap().to_vec();
                let old_sig = &module.signatures[old_calls[call.index()].index()];
                let mut k = usize::from(old_sig.dest.is_some());
                let mut passed = Vec::with_capacity(args.len());
                let output = match plan.result {
                    Change::Place(_) => unreachable!("a result is never lent"),
                    Change::Keep => {
                        if old_sig.dest.is_some() {
                            passed.push(args[0]);
                        }
                        None
                    }
                    Change::Nothing => None,
                    Change::Value(t, _) => {
                        let Arg::Address(at) = args[0] else {
                            unreachable!()
                        };
                        let v = c.value(t);
                        Some((at, v))
                    }
                };
                for change in &plan.params {
                    let arg = args[k];
                    k += 1;
                    match *change {
                        Change::Keep => passed.push(arg),
                        Change::Place(t) => {
                            let Arg::Address(at) = arg else {
                                unreachable!()
                            };
                            passed.push(Arg::Address(c.field(at, t)));
                        }
                        Change::Nothing => {}
                        Change::Value(t, _) => {
                            let Arg::Address(at) = arg else {
                                unreachable!()
                            };
                            passed.push(Arg::Value(c.load(at, t)));
                        }
                    }
                }
                c.body.calls[call.index()].args = PoolRange::push(&mut c.body.pools.args, passed);
                if let Some((_, value)) = output {
                    c.body.calls[call.index()].results =
                        PoolRange::push(&mut c.body.pools.values, [value]);
                }
                c.emit(old[i].clone());
                if let Some((address, value)) = output {
                    c.store(address, value);
                }
            } else {
                c.emit(old[i].clone());
            }
        }
        if matches!(c.body.terminators[bi], Terminator::Return(_))
            && let Some((address, t)) = scalar_dest
        {
            c.anchor = c.body.blocks[bi].anchor;
            let value = c.load(address, t);
            c.body.terminators[bi] =
                Terminator::Return(PoolRange::push(&mut c.body.pools.values, [value]));
        }
        c.body.blocks[bi].insts = PoolRange::new(start, c.insts.len() as u32 - start);
    }
    let dest = scalar_dest.map(
        |(address, _)| match c.body.addresses[address.index()].base {
            AddressBase::Slot(s) => s,
            _ => unreachable!(),
        },
    );
    c.body.insts = c.insts;
    c.body.inst_anchors = c.anchors;
    dest
}

/// A place whose whole value is one scalar. A fieldless word variant's case
/// operations become typed word reads/writes before its slot is promoted.
fn place_scalar(module: &Module, place: PlaceTy) -> Option<Ty> {
    match place {
        PlaceTy::Value(t) => Some(t),
        PlaceTy::Declared(row) => {
            let shape = &module.types[row.index()];
            let Carried::Value(t) = shape.carried else {
                return None;
            };
            match &shape.body {
                hirsel_core_layout::Body::Record(_) => Some(t),
                hirsel_core_layout::Body::Variant(cases)
                    if cases.iter().all(|c| c.fields.is_empty()) =>
                {
                    Some(t)
                }
                _ => None,
            }
        }
    }
}
fn width(t: Ty) -> u64 {
    match t {
        Ty::Scalar(s) => s.size(),
        Ty::Ptr => hirsel_core_layout::POINTER,
    }
}

mod places;
mod remap;
mod views;
use places::promote;
use remap::{addresses, compact, values};
use views::{scalar_copies, scalar_views};

mod numeric;
pub use numeric::Site as NumericSite;

/// Key recipes use the same scalar instruction stream in closed and generic code.
pub(super) fn inline_keys(module: &Module, body: &mut Body) {
    if key_glue::inline_ordered(module, body) {
        let aliases = (0..body.values.len())
            .map(|v| ValueId(v as u32))
            .collect::<Vec<_>>();
        let dead = vec![false; body.values.len()];
        let promoted = vec![false; body.slots.len()];
        compact(module, body, &aliases, &dead, &promoted, false);
    }
}
