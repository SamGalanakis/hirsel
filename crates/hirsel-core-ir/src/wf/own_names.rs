use super::*;
use crate::decode::Debit;

pub(super) type Steps = Vec<(u8, u32, u32)>;

/// A root and its static steps; `cut` when a run-time offset stands for some part
/// of the place before it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Pl {
    pub(super) root: usize,
    pub(super) steps: Steps,
    pub(super) cut: bool,
}

/// No loop of `pointer` and block parameters grows a place's steps past this.
const DEEP: usize = 16;

fn step(p: &mut Pl, s: &Step) -> bool {
    match *s {
        Step::Deref => return false,
        _ if p.cut => {}
        _ if p.steps.len() >= DEEP => p.cut = true,
        Step::Field(f) => p.steps.push((0, f, 0)),
        Step::CaseField { case, field } => p.steps.push((1, case, field)),
        Step::Seq(w) => p.steps.push((2, w as u32, 0)),
        Step::Index(_) | Step::Element(_) | Step::Byte(_) => p.cut = true,
    }
    true
}

pub(super) fn overlap(a: &Pl, b: &Pl) -> bool {
    a.root == b.root && (a.steps.starts_with(&b.steps) || b.steps.starts_with(&a.steps))
}

/// Whether writing `writer` overwrites all of `target`: either `writer` is `target`
/// or an ancestor of it, or every step from `target` to `writer` descends through
/// the sole field of a single-field non-resource record.
pub(super) fn covers(m: &Module, b: &Body, writer: &Pl, target: &Pl) -> bool {
    if writer.cut || writer.root != target.root {
        return false;
    }
    if target.steps.starts_with(&writer.steps) {
        return true;
    }
    if !writer.steps.starts_with(&target.steps) || target.root >= b.slots.len() {
        return false;
    }
    let PlaceTy::Declared(mut ty) = b.slots[target.root] else {
        return false;
    };
    let descend = |ty: TypeId, &(kind, a, c): &(u8, u32, u32)| -> Option<TypeId> {
        let shape = m.types.get(ty.index())?;
        let field = match kind {
            0 => shape.fields(None)?.get(a as usize)?,
            1 => shape.fields(Some(a))?.get(c as usize)?,
            _ => return None,
        };
        match field.ty {
            hirsel_core_layout::FieldTy::Inline(next) => Some(TypeId(next.0)),
            _ => None,
        }
    };
    for step in &target.steps {
        let Some(next) = descend(ty, step) else {
            return false;
        };
        ty = next;
    }
    let rest = &writer.steps[target.steps.len()..];
    for (idx, step) in rest.iter().enumerate() {
        if *step != (0, 0, 0) {
            return false;
        }
        let Some(shape) = m.types.get(ty.index()) else {
            return false;
        };
        let hirsel_core_layout::Body::Record(fields) = &shape.body else {
            return false;
        };
        if shape.repr == hirsel_core_layout::Repr::Resource || fields.len() != 1 {
            return false;
        }
        if idx + 1 < rest.len() {
            let hirsel_core_layout::FieldTy::Inline(next) = fields[0].ty else {
                return false;
            };
            ty = TypeId(next.0);
        }
    }
    true
}

/// What each address and value names, and the places an address's `deref` steps
/// read.
pub(super) struct Names {
    pub(super) addrs: Vec<Vec<Pl>>,
    pub(super) derefs: Vec<Vec<Pl>>,
    pub(super) vals: Vec<Vec<Pl>>,
}

/// Spread `vals` (and, when `slots`, the slots) through `pointer`s, addresses and,
/// along `edges` (argument, parameter), block parameters, to a fixpoint.
fn spread(
    b: &Body,
    mut vals: Vec<Vec<Pl>>,
    slots: bool,
    edges: &[(usize, usize)],
    scratch: &Debit<'_>,
) -> Result<Names, Error> {
    let mut addrs: Vec<Vec<Pl>> = Vec::new();
    scratch.reserve(&mut addrs, b.addresses.len())?;
    addrs.resize_with(b.addresses.len(), Vec::new);
    let mut derefs: Vec<Vec<Pl>> = Vec::new();
    scratch.reserve(&mut derefs, b.addresses.len())?;
    derefs.resize_with(b.addresses.len(), Vec::new);
    let mut pointers: Vec<(usize, usize)> = Vec::new();
    for i in &b.insts {
        if let Inst::Pointer { result, address } = *i {
            scratch.reserve(&mut pointers, 1)?;
            pointers.push((result.index(), address.index()));
        }
    }
    let union = |to: &mut Vec<Pl>, from: &[Pl]| -> Result<bool, Error> {
        let mut grew = false;
        for p in from {
            if !to.contains(p) {
                scratch.reserve(to, 1)?;
                scratch.allocation::<(u8, u32, u32)>(p.steps.len())?;
                to.push(p.clone());
                grew = true;
            }
        }
        Ok(grew)
    };
    loop {
        scratch.check()?;
        let mut grew = false;
        for (i, info) in b.addresses.iter().enumerate() {
            scratch.check()?;
            let base = match info.base {
                AddressBase::Slot(s) if slots => vec![Pl {
                    root: s.index(),
                    steps: vec![],
                    cut: false,
                }],
                AddressBase::Slot(_) => vec![],
                AddressBase::Address(x) => addrs[x.index()].clone(),
                AddressBase::Value(v) => vals[v.index()].clone(),
            };
            let steps = info.steps.get(&b.pools.steps).unwrap_or_default();
            let mut out = vec![];
            let mut read = vec![];
            for mut p in base {
                let mut whole = true;
                for s in steps {
                    if !step(&mut p, s) {
                        read.push(p.clone());
                        whole = false;
                        break;
                    }
                }
                if whole {
                    out.push(p);
                }
            }
            grew |= union(&mut addrs[i], &out)?;
            union(&mut derefs[i], &read)?;
        }
        for &(v, a) in &pointers {
            let from = addrs[a].clone();
            grew |= union(&mut vals[v], &from)?;
        }
        for &(a, p) in edges {
            let from = vals[a].clone();
            grew |= union(&mut vals[p], &from)?;
        }
        if !grew {
            return Ok(Names {
                addrs,
                derefs,
                vals,
            });
        }
    }
}

/// The places of `entry`'s values and of the body's slots, and the joins: each
/// pointer block parameter whose edges carry more than one place, or one place and
/// a pointer that names none, gets a root of
/// its own (after the slots and entry values), what it points at on the path
/// taken, so that a drop through it is strong and its state follows its edge.
/// An entry pointer parameter is never a join: an edge into the entry block must
/// give it back itself.
pub(super) fn names(
    b: &Body,
    entry: &[ValueId],
    scratch: &Debit<'_>,
) -> Result<(Names, Names, Vec<usize>), Error> {
    let slots = b.slots.len();
    let mut vals: Vec<Vec<Pl>> = Vec::new();
    scratch.reserve(&mut vals, b.values.len())?;
    vals.resize_with(b.values.len(), Vec::new);
    for (k, v) in entry.iter().enumerate() {
        scratch.reserve(&mut vals[v.index()], 1)?;
        vals[v.index()].push(Pl {
            root: slots + k,
            steps: vec![],
            cut: false,
        });
    }
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for e in &b.edges {
        for (a, p) in ptr_args(b, e) {
            // The entry block's pointer parameters are the caller's places: the
            // join rule below does not visit it, so no edge may rebind one.
            need(e.target.index() != 0 || a == p, || {
                format!("an edge into b0 gives pointer parameter v{} v{}", p.0, a.0)
            })
            .map_err(Error::Invalid)?;
            scratch.reserve(&mut edges, 1)?;
            edges.push((a.index(), p.index()));
        }
    }
    let under = spread(b, vals, true, &edges, scratch)?;
    // A parameter that names one place is still a join when an edge may carry it a
    // pointer naming none (a heap element, a call's result) or one made from a join:
    // it is that place only on some paths, so no write or drop through it is the
    // place's.
    let mut joins: Vec<usize> = Vec::new();
    // Whether a value is a join's parameter or a `pointer` of an address based on one.
    let mut joined = Vec::new();
    scratch.reserve(&mut joined, b.values.len())?;
    joined.resize(b.values.len(), false);
    let based = |a: AddressId| {
        let mut at = a;
        loop {
            match b.addresses[at.index()].base {
                AddressBase::Address(x) => at = x,
                AddressBase::Value(v) => return Some(v.index()),
                AddressBase::Slot(_) => return None,
            }
        }
    };
    loop {
        scratch.check()?;
        let mut grew = false;
        for i in &b.insts {
            if let Inst::Pointer { result, address } = *i
                && !joined[result.index()]
                && based(address).is_some_and(|v| joined[v])
            {
                joined[result.index()] = true;
                grew = true;
            }
        }
        for k in b.blocks.iter().skip(1) {
            for p in k.params.get(&b.pools.values).unwrap_or_default() {
                let idx = p.index();
                if joined[idx] || b.values[idx] != Ty::Ptr || under.vals[idx].is_empty() {
                    continue;
                }
                let mixed = || {
                    edges
                        .iter()
                        .any(|&(a, to)| to == idx && (under.vals[a].is_empty() || joined[a]))
                };
                if under.vals[idx].len() > 1 || mixed() {
                    scratch.reserve(&mut joins, 1)?;
                    joins.push(idx);
                    joined[idx] = true;
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut seed: Vec<Vec<Pl>> = Vec::new();
    scratch.reserve(&mut seed, b.values.len())?;
    seed.resize_with(b.values.len(), Vec::new);
    for (k, &p) in joins.iter().enumerate() {
        scratch.reserve(&mut seed[p], 1)?;
        seed[p].push(Pl {
            root: slots + entry.len() + k,
            steps: vec![],
            cut: false,
        });
    }
    // An edge into a join's parameter carries no join: every parameter an edge
    // gives a join's pointer to is itself a join, seeded above.
    let joined = spread(b, seed, false, &[], scratch)?;
    Ok((under, joined, joins))
}

/// A local slot holds a frame-owned value (cannot be escaped in a returned dest view)
/// when it is dropped in this body, written as the destination of an unanchored call,
/// or when the enclosing function itself declares no result anchors.
pub(super) fn local_owned_slots(
    m: &Module,
    sig: &Signature,
    b: &Body,
    n: &Names,
    scratch: &Debit<'_>,
) -> Result<Vec<bool>, Error> {
    let mut owned = Vec::new();
    scratch.reserve(&mut owned, b.slots.len())?;
    owned.resize(b.slots.len(), sig.anchors.is_empty());
    for i in &b.insts {
        match *i {
            Inst::Drop { address, .. } => {
                for r in &n.addrs[address.index()] {
                    if r.root < b.slots.len() {
                        owned[r.root] = true;
                    }
                }
            }
            Inst::Call { call } => {
                let site = &b.calls[call.index()];
                let csig = &m.signatures[site.signature.index()];
                if csig.dest.is_some() && csig.anchors.is_empty() {
                    let args = site.args.get(&b.pools.args).unwrap_or_default();
                    if let Some(&Arg::Address(d)) = args.first() {
                        for r in &n.addrs[d.index()] {
                            if r.root < b.slots.len() {
                                owned[r.root] = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(owned)
}

/// Compare call arguments after spreading pointer names through incoming edges.
pub(super) fn check_call(
    m: &Module,
    b: &Body,
    site: &CallSite,
    n: &Names,
    scratch: &Debit<'_>,
) -> Result<(), Error> {
    let sig = &m.signatures[site.signature.index()];
    let args = site.args.get(&b.pools.args).unwrap_or_default();
    let dest = usize::from(sig.dest.is_some());
    for (i, arg) in args.iter().enumerate() {
        let Arg::Address(a) = *arg else { continue };
        if i >= dest
            && !matches!(
                sig.params[i - dest],
                Param::Address {
                    loan: Loan::Mut,
                    ..
                }
            )
        {
            continue;
        }
        for (j, other) in args.iter().enumerate() {
            if i == j {
                continue;
            }
            let places = match *other {
                Arg::Address(b) => &n.addrs[b.index()],
                Arg::Value(v) => &n.vals[v.index()],
            };
            for p in &n.addrs[a.index()] {
                scratch.check()?;
                if places.iter().any(|q| overlap(p, q)) {
                    return Err(Error::Invalid(format!(
                        "a{}: a mut place overlaps argument {j}",
                        a.0
                    )));
                }
            }
        }
    }
    Ok(())
}

/// FORM #112 is memmove of plain bytes, including overlapping ranges.
pub(super) fn check_moves(m: &Module, b: &Body, scratch: &Debit<'_>) -> Result<(), Error> {
    let mut recipes = None;
    for inst in &b.insts {
        let Inst::Move { from, to, .. } = *inst else {
            continue;
        };
        for a in [from, to] {
            let t = match b.addresses[a.index()].ty {
                PlaceTy::Declared(t) => t,
                // A scalar's bytes own nothing; a pointer may be an owner.
                PlaceTy::Value(Ty::Ptr) => {
                    return Err(Error::Invalid(format!(
                        "a{}: move requires plain operands",
                        a.0
                    )));
                }
                PlaceTy::Value(_) => continue,
            };
            let layouts = match &recipes {
                Some(layouts) => layouts,
                None => recipes.insert(
                    hirsel_core_layout::compute_budgeted(&m.types, |bytes| {
                        scratch.charge_bytes(bytes)
                    })
                    .map_err(|err| match err {
                        hirsel_core_layout::ComputeError::Budget(e) => Error::from(e),
                        hirsel_core_layout::ComputeError::Layout(e) => {
                            Error::Invalid(format!("layout: {e:?}"))
                        }
                    })?,
                ),
            };
            if !layouts.get(t.index()).is_some_and(|recipe| recipe.plain) {
                return Err(Error::Invalid(format!(
                    "a{}: move requires plain operands",
                    a.0
                )));
            }
        }
    }
    Ok(())
}

/// An edge's pointer arguments and the target's parameters they bind.
pub(super) fn ptr_args(b: &Body, e: &Edge) -> Vec<(ValueId, ValueId)> {
    let params = b.blocks[e.target.index()]
        .params
        .get(&b.pools.values)
        .unwrap_or_default();
    let args = e.args.get(&b.pools.values).unwrap_or_default();
    args.iter()
        .zip(params)
        .filter(|(_, p)| b.values[p.index()] == Ty::Ptr)
        .map(|(a, p)| (*a, *p))
        .collect()
}

/// A dest view's anchors (OWN-20) name, ascending, address parameters of a
/// signature with a `dest`.
pub(super) fn anchors(s: &Signature) -> R {
    need(s.anchors.is_empty() || s.dest.is_some(), || {
        "anchors a result with no dest".into()
    })?;
    need(s.anchors.windows(2).all(|w| w[0] < w[1]), || {
        "anchors are not ascending".into()
    })?;
    for &k in &s.anchors {
        need(
            matches!(s.params.get(k as usize), Some(Param::Address { .. })),
            || format!("anchor {k} is not an address parameter"),
        )?;
    }
    Ok(())
}

/// Whether slot `s` is a `bool` value place (a drop flag's shape).
pub(super) fn is_flag_slot(b: &Body, s: usize) -> bool {
    b.slots.get(s)
        == Some(&PlaceTy::Value(Ty::Scalar(
            hirsel_core_layout::Scalar::Bool,
        )))
}
