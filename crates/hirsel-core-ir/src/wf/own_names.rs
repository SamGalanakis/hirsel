use super::*;
use crate::decode::Debit;

/// Steps by kind: `(0, field, 0)`, `(1, case, field)`, `(2, width, 0)`, the byte
/// windows `(3, start, end)`, exactly bytes `start..end` of the place before it,
/// and `(4, start, end)`, some part of them, and `(5, type, 0)`, the type an exact
/// window is read as. Only its type and static steps follow a window ([`window`]).
pub(super) type Steps = Vec<(u8, u32, u32)>;

/// A root and its static steps; `cut` when a run-time offset stands for some part
/// of the place before it, or a byte window ends the steps.
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

/// Per address, what a byte window needs: the sum of the `byte` steps by `u64`
/// constants it is made of (`Some(0)` for none), and its place's size, when
/// fixed, and declared type.
#[derive(Clone, Copy, Default)]
pub(super) struct Span {
    bytes: Option<u64>,
    size: Option<u64>,
    ty: Option<u32>,
}

/// Give `p` an address's steps through a byte window; `None` leaves them to
/// [`step`], `Some(false)` when a `deref` step reads `p`.
///
/// Constant `byte` steps from a place with only static steps, or from a window's
/// start (a `pointer` to it given a type, say), name exactly the bytes their sum
/// and the address type's size cover, `(3, start, end)`, and that type, `(5, t,
/// 0)`, after which static steps are kept as in any typed place. A place given
/// another type at no offset is the same place. An element of a window, or a
/// static step of an untyped one, lies inside it, `(4, start, end)`. A `byte`
/// step from inside a window may land anywhere in the place before the window.
fn window(p: &mut Pl, steps: &[Step], span: Span) -> Option<bool> {
    let open = |p: &mut Pl, start: u64| -> Option<()> {
        let end = start.checked_add(span.size?.max(1))?;
        let w = (3, u32::try_from(start).ok()?, u32::try_from(end).ok()?);
        if p.steps.len() + 2 > DEEP {
            return None;
        }
        p.steps.push(w);
        p.steps.extend(span.ty.map(|t| (5, t, 0)));
        p.cut = true;
        Some(())
    };
    let at = p.steps.iter().rposition(|&(k, ..)| matches!(k, 3 | 4));
    let Some(w) = at.filter(|_| p.cut) else {
        if !p.cut && !steps.is_empty() {
            let mut q = p.clone();
            open(&mut q, span.bytes?)?;
            *p = q;
            return Some(true);
        }
        return None;
    };
    let (k, s, e) = p.steps[w];
    let bare = k == 3 && p.steps[w + 1..].iter().all(|&(k, ..)| k == 5);
    if let Some(c) = span.bytes {
        if steps.is_empty() && !bare {
            return Some(true);
        }
        p.steps.truncate(w);
        if bare && let Some(start) = u64::from(s).checked_add(c) {
            open(p, start)?;
            return Some(true);
        }
        return None;
    }
    let typed = p.steps.get(w + 1).is_some_and(|&(k, ..)| k == 5);
    let mut kind = k;
    for st in steps {
        match *st {
            Step::Deref => return Some(false),
            Step::Byte(_) if kind != 0 => {
                p.steps.truncate(w);
                kind = 0;
            }
            _ if kind != 3 => {}
            Step::Field(f) if typed && p.steps.len() < DEEP => p.steps.push((0, f, 0)),
            Step::CaseField { case, field } if typed && p.steps.len() < DEEP => {
                p.steps.push((1, case, field))
            }
            Step::Seq(x) if typed && p.steps.len() < DEEP => p.steps.push((2, x as u32, 0)),
            _ => {
                p.steps.truncate(w);
                p.steps.push((4, s, e));
                kind = 4;
            }
        }
    }
    Some(true)
}

/// Whether two places may share a byte: one's steps lead to the other's, or the
/// first step they differ in is two byte windows that share a byte, or anything
/// but two static steps.
pub(super) fn overlap(a: &Pl, b: &Pl) -> bool {
    a.root == b.root
        && a.steps
            .iter()
            .zip(&b.steps)
            .find(|(x, y)| x != y)
            .is_none_or(|(&(k, s, e), &(l, t, f))| match (k, l) {
                (3 | 4, 3 | 4) => s < f && t < e,
                (0..=2, 0..=2) => false,
                _ => true,
            })
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
    spans: &[Span],
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
                match window(&mut p, steps, spans.get(i).copied().unwrap_or_default()) {
                    Some(true) => {
                        out.push(p);
                        continue;
                    }
                    Some(false) => {
                        read.push(p);
                        continue;
                    }
                    None => {}
                }
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
    m: &Module,
    b: &Body,
    entry: &[ValueId],
    scratch: &Debit<'_>,
) -> Result<(Names, Names, Vec<usize>), Error> {
    let spans = spans(m, b, scratch)?;
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
    let under = spread(b, vals, true, &edges, &spans, scratch)?;
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
    let joined = spread(b, seed, false, &[], &spans, scratch)?;
    Ok((under, joined, joins))
}

/// Each address's [`Span`]; none when no address has a constant `byte` step.
fn spans(m: &Module, b: &Body, scratch: &Debit<'_>) -> Result<Vec<Span>, Error> {
    let mut consts = std::collections::HashMap::new();
    for i in &b.insts {
        if let Inst::Const { result, bits } = *i
            && b.values[result.index()] == Ty::Scalar(Scalar::U64)
        {
            scratch.allocation::<(usize, u64)>(1)?;
            consts.insert(result.index(), bits);
        }
    }
    let bytes = |a: &AddressInfo| {
        a.steps
            .get(&b.pools.steps)
            .unwrap_or_default()
            .iter()
            .try_fold(0u64, |at, s| match *s {
                Step::Byte(v) => at.checked_add(*consts.get(&v.index())?),
                _ => None,
            })
    };
    let windowed = b.addresses.iter().any(|a| {
        let steps = a.steps.get(&b.pools.steps).unwrap_or_default();
        !steps.is_empty() && bytes(a).is_some()
    });
    let mut spans = Vec::new();
    if !windowed {
        return Ok(spans);
    }
    let recipes = layouts(m, scratch)?;
    scratch.reserve(&mut spans, b.addresses.len())?;
    for a in &b.addresses {
        let size = match a.ty {
            PlaceTy::Value(Ty::Scalar(s)) => Some(s.size()),
            PlaceTy::Value(Ty::Ptr) => Some(8),
            PlaceTy::Value(Ty::Vector(_)) => None,
            // A resource or region place holds its pointer.
            PlaceTy::Declared(t)
                if matches!(
                    m.types.get(t.index()).map(|r| &r.repr),
                    Some(hirsel_core_layout::Repr::Resource | hirsel_core_layout::Repr::Region)
                ) =>
            {
                Some(8)
            }
            PlaceTy::Declared(t) => match recipes.get(t.index()).map(|r| &r.layout) {
                Some(hirsel_core_layout::Layout::Fixed(f)) => Some(f.size),
                _ => None,
            },
        };
        spans.push(Span {
            bytes: bytes(a),
            size,
            ty: match a.ty {
                PlaceTy::Declared(t) => Some(t.0),
                PlaceTy::Value(_) => None,
            },
        });
    }
    Ok(spans)
}

/// The module's type layouts, charged to `scratch`.
fn layouts(m: &Module, scratch: &Debit<'_>) -> Result<Vec<hirsel_core_layout::RecipeRow>, Error> {
    hirsel_core_layout::compute_budgeted(&m.types, |bytes| scratch.charge_bytes(bytes)).map_err(
        |err| match err {
            hirsel_core_layout::ComputeError::Budget(e) => Error::from(e),
            hirsel_core_layout::ComputeError::Layout(e) => Error::Invalid(format!("layout: {e:?}")),
        },
    )
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
                None => recipes.insert(layouts(m, scratch)?),
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
