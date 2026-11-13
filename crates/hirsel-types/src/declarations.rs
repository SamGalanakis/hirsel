//! Declaration proofs for equality and held authority (GEN-1/CAP-2).
type IdSet<T> = std::collections::HashSet<T, hirsel_intern::IdHasher>;
use crate::*;

/// Data/Key are representation properties. Parameter evidence belongs to the
/// occurrence's lexical scope; a bare ordinal cannot select that scope.
/// Missing declarations and recovery prove nothing. Non-regular polymorphic
/// recursion is an operational tooling failure, never negative evidence.
pub fn structural_equality(
    source: &TypeInterner,
    root: TypeId,
    bound: Bound,
    declarations: &GenericDeclarations,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    assert!(matches!(bound, Bound::Data | Bound::Key));
    let proves = |bounds: &[Bound]| {
        bounds.contains(&bound) || (bound == Bound::Data && bounds.contains(&Bound::Key))
    };
    if source.get(root).is_none() {
        return Ok(false);
    }
    let store = source;
    let mut pending = vec![(root, false)];
    let mut seen = IdSet::default();
    let mut nominals = NominalExpansions::default();
    while let Some((id, exit)) = pending.pop() {
        if exit {
            nominals.leave(id, 0);
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(row) = store.get(id).cloned() else {
            return Ok(false);
        };
        match row.kind {
            TypeKind::Error | TypeKind::Function(_) | TypeKind::Primitive(Primitive::Never) => {
                return Ok(false);
            }
            TypeKind::Primitive(Primitive::F32 | Primitive::F64) if bound == Bound::Key => {
                return Ok(false);
            }
            TypeKind::Primitive(_) => (),
            // A declaration parameter without a supplied actual has no proof.
            TypeKind::Parameter(p) => {
                if !proves(declarations.bounds(p)) {
                    return Ok(false);
                }
            }
            TypeKind::Named {
                definition,
                arguments,
            } => {
                let Some(signature) = lookup(definition)? else {
                    return Ok(false);
                };
                let SignatureKind::Type(declaration) = &signature.kind else {
                    return Ok(false);
                };
                if declaration.resource || declaration.capability.is_some() {
                    return Ok(false);
                }
                if proves(&declaration.bounds) {
                    continue;
                }
                if signature.metadata.visibility == hirsel_syntax::Visibility::PublicOpaque {
                    return Ok(false);
                }
                if !nominals.enter(id, 0, store)? {
                    continue;
                }
                pending.push((id, true));
                let Some(representation) = declaration.representation else {
                    return Ok(false);
                };
                let Ok(representation) = crate::graph::expand(
                    store,
                    representation,
                    &arguments,
                    &signature.metadata.roles,
                ) else {
                    return Ok(false);
                };
                pending.push((representation, false));
            }
            TypeKind::Builtin { kind, arguments } => {
                // GEN-1, spec #41: Shared/Slots lack structural operations;
                // SortedMap owns its function-bearing Order dictionary.
                if !matches!(
                    kind,
                    BuiltinType::Array
                        | BuiltinType::List
                        | BuiltinType::Inline
                        | BuiltinType::Simd
                        | BuiltinType::Packed
                        | BuiltinType::Map
                        | BuiltinType::Set
                ) {
                    return Ok(false);
                }
                pending.extend(arguments.into_iter().filter_map(|a| match a {
                    GenericArgument::Type(t) => Some((t, false)),
                    GenericArgument::Size(_) => None,
                }));
            }
            TypeKind::Tuple(ts) => pending.extend(ts.into_iter().map(|t| (t, false))),
            TypeKind::Record(fs) => pending.extend(fs.into_iter().map(|f| (f.ty, false))),
            TypeKind::Variant(vs) => pending.extend(
                vs.into_iter()
                    .flat_map(|v| v.fields.into_iter().map(|f| (f.1, false))),
            ),
            TypeKind::Box(t) => pending.push((t, false)),
        }
    }
    Ok(true)
}

/// Whether bounds supply canonical order (GEN-1): `ordered`, or `ordered of`, which also
/// requires a comparison domain at every use (SEM-5).
pub fn bounds_order(bounds: &[Bound]) -> bool {
    bounds
        .iter()
        .any(|b| matches!(b, Bound::Ordered | Bound::OrderedOf))
}

/// What an `ordered` proof admits for comparison-domain tokens (SEM-5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Domains {
    /// A use: `ordered of` tokens qualify; `comparison_domains` checks their brands.
    Carried,
    /// A declaration's representation, where no field can name one domain for
    /// every value: an `ordered of` token disqualifies it. The caller's declarations
    /// treat its own parameters as ordered, since every use supplies ordered arguments,
    /// and the declaration itself (named here) as ordered where it recurs.
    Declaration(Option<DefId>),
}

/// GEN-1 `ordered`: canonical order derived from representation. Integers, bool, char,
/// String, Bytes and unit qualify; tuples, boxes, Array, List, Inline and Packed when their
/// components do; a nominal only by its explicit `ordered` or `ordered of` bound, with every
/// type argument ordered. Floats, functions, Map, Set, Simd, handles without the bound and
/// capabilities never qualify. Missing declarations and recovery prove nothing.
pub fn structural_ordered(
    source: &TypeInterner,
    root: TypeId,
    domains: Domains,
    declarations: &GenericDeclarations,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    let mut pending = vec![root];
    let mut seen = IdSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(row) = source.get(id).cloned() else {
            return Ok(false);
        };
        match row.kind {
            TypeKind::Error
            | TypeKind::Function(_)
            | TypeKind::Primitive(Primitive::Never | Primitive::F32 | Primitive::F64) => {
                return Ok(false);
            }
            TypeKind::Primitive(_) => (),
            TypeKind::Parameter(p) => {
                if !bounds_order(declarations.bounds(p)) {
                    return Ok(false);
                }
            }
            TypeKind::Named {
                definition,
                arguments,
            } if domains == Domains::Declaration(Some(definition)) => {
                pending.extend(arguments.iter().filter_map(|a| match a {
                    GenericArgument::Type(t) => Some(*t),
                    GenericArgument::Size(_) => None,
                }));
            }
            TypeKind::Named {
                definition,
                arguments,
            } => {
                let Some(signature) = lookup(definition)? else {
                    return Ok(false);
                };
                let SignatureKind::Type(declaration) = &signature.kind else {
                    return Ok(false);
                };
                if declaration.resource
                    || declaration.capability.is_some()
                    || !bounds_order(&declaration.bounds)
                    || (matches!(domains, Domains::Declaration(_))
                        && declaration.bounds.contains(&Bound::OrderedOf))
                {
                    return Ok(false);
                }
                pending.extend(arguments.iter().filter_map(|a| match a {
                    GenericArgument::Type(t) => Some(*t),
                    GenericArgument::Size(_) => None,
                }));
            }
            TypeKind::Builtin { kind, arguments } => {
                if !matches!(
                    kind,
                    BuiltinType::Array
                        | BuiltinType::List
                        | BuiltinType::Inline
                        | BuiltinType::Packed
                ) {
                    return Ok(false);
                }
                pending.extend(arguments.iter().filter_map(|a| match a {
                    GenericArgument::Type(t) => Some(*t),
                    GenericArgument::Size(_) => None,
                }));
            }
            TypeKind::Tuple(ts) => pending.extend(ts.iter().copied()),
            TypeKind::Record(fs) => pending.extend(fs.iter().map(|f| f.ty)),
            TypeKind::Variant(vs) => {
                pending.extend(vs.iter().flat_map(|v| v.fields.iter().map(|f| f.1)))
            }
            TypeKind::Box(t) => pending.push(t),
        }
    }
    Ok(true)
}

/// GEN-1: a nominal's explicit `ordered` or `ordered of` bound holds when its
/// representation is ordered with no `ordered of` part, its own parameters ordered (every
/// use supplies ordered arguments). The compiler derives the order from that
/// representation; no comparator or projection can discharge it.
pub fn ordered_declaration(
    source: &TypeInterner,
    this: DefId,
    generics: &[GenericParameter],
    representation: TypeId,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    let own: Vec<_> = generics
        .iter()
        .map(|g| match g {
            GenericParameter::Type { name, bounds } => GenericParameter::Type {
                name: name.clone(),
                bounds: bounds
                    .iter()
                    .copied()
                    .chain([Bound::Ordered])
                    .collect::<Vec<_>>()
                    .into(),
            },
            size => size.clone(),
        })
        .collect();
    structural_ordered(
        source,
        representation,
        Domains::Declaration(Some(this)),
        &GenericDeclarations::signature(&own),
        lookup,
    )
}

/// SEM-5/PROV-2: two compared types establish one comparison domain at every payload
/// depth. Each `ordered of` occurrence on either side has exactly one brand, the same as
/// its counterpart's; an absent (forgotten or never known) or union brand fails. Comparing
/// a type with itself checks that every domain it carries is known, as a `[T: ordered]`
/// substitution requires. Structurally different operands are not this check's concern
/// and fail it.
pub fn comparison_domains(
    source: &TypeInterner,
    left: TypeId,
    right: TypeId,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    domain_walk(source, left, right, true, lookup)
}

/// PROV-2 once callee results carry the caller's origins: whether some `ordered of`
/// part known on both sides carries a union brand or two different owners. Unlike
/// `comparison_domains`, it skips a part that is unbranded or structurally unpaired,
/// which the per-operand check owns.
pub fn comparison_domain_conflict(
    source: &TypeInterner,
    left: TypeId,
    right: TypeId,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    domain_walk(source, left, right, false, lookup).map(|agree| !agree)
}

fn domain_walk(
    source: &TypeInterner,
    left: TypeId,
    right: TypeId,
    strict: bool,
    lookup: &mut impl FnMut(DefId) -> Result<Option<SignatureRef>, QueryError>,
) -> Result<bool, QueryError> {
    let mut pending = vec![(left, right)];
    let mut seen = std::collections::HashSet::new();
    while let Some((a, b)) = pending.pop() {
        if !seen.insert((a, b)) {
            continue;
        }
        let (Some(ra), Some(rb)) = (source.get(a).cloned(), source.get(b).cloned()) else {
            if strict {
                return Ok(false);
            }
            continue;
        };
        let pair =
            |x: &[GenericArgument], y: &[GenericArgument]| -> Option<Vec<(TypeId, TypeId)>> {
                (x.len() == y.len()).then(|| {
                    x.iter()
                        .zip(y)
                        .filter_map(|p| match p {
                            (GenericArgument::Type(x), GenericArgument::Type(y)) => Some((*x, *y)),
                            _ => None,
                        })
                        .collect()
                })
            };
        match (&ra.kind, &rb.kind) {
            (
                TypeKind::Named {
                    definition: da,
                    arguments: xa,
                },
                TypeKind::Named {
                    definition: db,
                    arguments: xb,
                },
            ) if da == db => {
                let domain = match lookup(*da)? {
                    Some(signature) => matches!(&signature.kind,
                        SignatureKind::Type(d) if d.bounds.contains(&Bound::OrderedOf)),
                    None if strict => return Ok(false),
                    None => false,
                };
                if domain {
                    let [one, other] = [&ra.brand, &rb.brand].map(|b| &b.alternatives);
                    let known = !one.is_empty() && !other.is_empty();
                    if (strict || known) && (one.len() != 1 || one != other) {
                        return Ok(false);
                    }
                }
                let Some(parts) = pair(xa, xb) else {
                    if strict {
                        return Ok(false);
                    }
                    continue;
                };
                pending.extend(parts);
            }
            (
                TypeKind::Builtin {
                    kind: ka,
                    arguments: xa,
                },
                TypeKind::Builtin {
                    kind: kb,
                    arguments: xb,
                },
            ) if ka == kb => {
                let Some(parts) = pair(xa, xb) else {
                    if strict {
                        return Ok(false);
                    }
                    continue;
                };
                pending.extend(parts);
            }
            (TypeKind::Tuple(xa), TypeKind::Tuple(xb)) if xa.len() == xb.len() => {
                pending.extend(xa.iter().copied().zip(xb.iter().copied()))
            }
            (TypeKind::Record(xa), TypeKind::Record(xb)) if xa.len() == xb.len() => {
                pending.extend(xa.iter().map(|f| f.ty).zip(xb.iter().map(|f| f.ty)))
            }
            (TypeKind::Variant(xa), TypeKind::Variant(xb)) if xa.len() == xb.len() => {
                for (va, vb) in xa.iter().zip(xb.iter()) {
                    if va.fields.len() != vb.fields.len() {
                        if strict {
                            return Ok(false);
                        }
                        continue;
                    }
                    pending.extend(
                        va.fields
                            .iter()
                            .map(|f| f.1)
                            .zip(vb.fields.iter().map(|f| f.1)),
                    );
                }
            }
            (TypeKind::Box(x), TypeKind::Box(y)) => pending.push((*x, *y)),
            (TypeKind::Primitive(x), TypeKind::Primitive(y)) if x == y => (),
            (TypeKind::Parameter(x), TypeKind::Parameter(y)) if x == y => (),
            _ if !strict => (),
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// These bounds exclude held authority, independently of copy/sendable modes.
pub fn bounds_imply_inert(bounds: &[Bound]) -> bool {
    bounds.iter().any(|b| {
        matches!(
            b,
            Bound::Inert | Bound::Flat | Bound::Data | Bound::Key | Bound::Serializable
        )
    })
}

/// Held authority comes from declarations. The producer publishes field joins
/// union explicit uses; consumers specialize that summary, never re-open fields.
/// A capability intrinsically contributes itself (CAP-2); an ambient heap token
/// is the declared exception. Phantom arguments do not contribute authority.
///
/// `lookup` receives the occurrence and its nominal path, or None for a builtin
/// with an optional resolver-selected declaration. `declarations` supplies
/// exact lexical evidence. Callable parameter references remain symbolic: evaluating
/// them against checked value bindings belongs to the authority pass.
pub fn structural_authority<E>(
    store: &TypeInterner,
    root: TypeId,
    roles: &NominalRoles,
    declarations: &GenericDeclarations,
    lookup: &mut impl FnMut(TypeId, Option<DefId>) -> Result<Option<SignatureRef>, E>,
) -> Result<AuthoritySummary, E> {
    let mut pending = vec![root];
    let mut seen = IdSet::default();
    let mut summary = AuthoritySummary::inert();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(row) = store.get(id) else {
            return Ok(AuthoritySummary::Unknown);
        };
        if row.is_heap_target(roles) {
            continue;
        }
        match &row.kind {
            TypeKind::Error => return Ok(AuthoritySummary::Unknown),
            TypeKind::Primitive(_) => (),
            TypeKind::Parameter(p) => {
                if !bounds_imply_inert(declarations.bounds(*p)) {
                    summary = summary.join(AuthoritySummary::Known(
                        vec![AuthorityTerm::TypeParameter(*p)].into(),
                    ));
                }
            }
            TypeKind::Function(f) => summary = summary.join(f.authority.clone()),
            TypeKind::Named {
                definition,
                arguments,
            } => {
                let Some(signature) = lookup(id, Some(*definition))? else {
                    return Ok(AuthoritySummary::Unknown);
                };
                let SignatureKind::Type(declaration) = &signature.kind else {
                    return Ok(AuthoritySummary::Unknown);
                };
                if declaration.capability.is_some() {
                    summary = summary.join(AuthoritySummary::Known(
                        vec![AuthorityTerm::Capability(definition.path().clone())].into(),
                    ));
                    // CAP-5: actuals and owner ceilings do not refine the
                    // nominal permission of a whole seal.
                    continue;
                }
                if !specialize_authority(
                    &declaration.authority,
                    arguments,
                    &mut summary,
                    &mut pending,
                ) {
                    return Ok(AuthoritySummary::Unknown);
                }
                // CAP-2: an erased carrier also holds its value's own erased
                // evidence, unknown for a written type, as an Iter does.
                if declaration.captures.is_some() {
                    summary = summary.join(
                        row.erased_captures
                            .as_ref()
                            .map(ErasedCaptures::authority)
                            .unwrap_or(AuthoritySummary::Unknown),
                    );
                }
            }
            TypeKind::Builtin { kind, arguments } => {
                if *kind == BuiltinType::Iter {
                    summary = summary.join(
                        row.erased_captures
                            .as_ref()
                            .map(ErasedCaptures::authority)
                            .unwrap_or(AuthoritySummary::Unknown),
                    );
                } else if let Some(signature) = lookup(id, None)? {
                    let SignatureKind::Type(declaration) = &signature.kind else {
                        return Ok(AuthoritySummary::Unknown);
                    };
                    if !specialize_authority(
                        &declaration.authority,
                        arguments,
                        &mut summary,
                        &mut pending,
                    ) {
                        return Ok(AuthoritySummary::Unknown);
                    }
                } else {
                    summary = summary.join(builtin_held_authority(*kind, roles));
                    if !matches!(
                        kind,
                        BuiltinType::Ptr | BuiltinType::FnPtr | BuiltinType::SlotId
                    ) {
                        pending.extend(arguments.iter().filter_map(|a| match a {
                            GenericArgument::Type(t) => Some(*t),
                            GenericArgument::Size(_) => None,
                        }));
                    }
                }
            }
            TypeKind::Tuple(ts) => pending.extend(ts),
            TypeKind::Record(fs) => pending.extend(fs.iter().map(|f| f.ty)),
            TypeKind::Variant(vs) => {
                pending.extend(vs.iter().flat_map(|v| v.fields.iter().map(|f| f.1)))
            }
            TypeKind::Box(t) => pending.push(*t),
        }
    }
    Ok(summary)
}
fn specialize_authority(
    declaration: &AuthoritySummary,
    arguments: &[GenericArgument],
    summary: &mut AuthoritySummary,
    pending: &mut Vec<TypeId>,
) -> bool {
    let AuthoritySummary::Known(terms) = declaration else {
        return false;
    };
    for term in terms {
        match term {
            AuthorityTerm::TypeParameter(p) if p.binder() == GenericBinder::Bound(0) => {
                match arguments.get(p.parameter().0 as usize) {
                    Some(GenericArgument::Type(t)) => pending.push(*t),
                    _ => return false,
                }
            }
            term => {
                *summary = summary
                    .clone()
                    .join(AuthoritySummary::Known(vec![term.clone()].into()))
            }
        }
    }
    true
}
