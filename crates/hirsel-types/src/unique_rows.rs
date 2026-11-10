//! The unique rows of a declared result type (`-> unique T`, a unique
//! `Result`/`Option` payload or tuple element): the structure a caller reads
//! from a declared result contract (MODE-2). Nothing here is body-derived.
/// A result header row, without traversing arbitrary nominal representations.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
pub enum UniqueResultRow {
    Root,
    Tuple(u32),
    Payload { variant: u32, field: u32 },
}

/// Direct rows, with their declared types. Result uses the registered identity;
/// Option uses the canonical core declaration. Arbitrary nominal fields stay opaque.
pub fn result_rows(
    types: &crate::TypeInterner,
    root: crate::TypeId,
    roles: &crate::NominalRoles,
    lookup: &mut impl FnMut(hirsel_syntax::DefId) -> Option<crate::SignatureRef>,
) -> Vec<(UniqueResultRow, crate::TypeId)> {
    let mut rows = vec![(UniqueResultRow::Root, root)];
    rows.extend(
        result_parts(types, root, roles, lookup)
            .into_iter()
            .map(|(step, ty)| {
                (
                    match step {
                        ResultStep::Element(i) => UniqueResultRow::Tuple(i),
                        ResultStep::Payload { variant, field } => {
                            UniqueResultRow::Payload { variant, field }
                        }
                    },
                    ty,
                )
            }),
    );
    rows
}

/// One step from a result carrier to a part: a tuple element, or a field of
/// a `Result`/`Option` alternative.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
pub enum ResultStep {
    Element(u32),
    Payload { variant: u32, field: u32 },
}

/// The direct parts of a result carrier, with their declared types: tuple
/// elements and `Result`/`Option` payloads. Any other type has none here.
pub fn result_parts(
    types: &crate::TypeInterner,
    ty: crate::TypeId,
    roles: &crate::NominalRoles,
    lookup: &mut impl FnMut(hirsel_syntax::DefId) -> Option<crate::SignatureRef>,
) -> Vec<(ResultStep, crate::TypeId)> {
    use crate::{SignatureKind, TypeKind};
    let mut parts = Vec::new();
    match &types.row(ty).kind {
        TypeKind::Tuple(elements) => parts.extend(
            elements
                .iter()
                .enumerate()
                .map(|(i, ty)| (ResultStep::Element(i as u32), *ty)),
        ),
        TypeKind::Named {
            definition,
            arguments,
        } if roles.result_type.as_ref() == Some(definition.path())
            || definition.path() == &hirsel_syntax::DefPath::core("Option") =>
        {
            if let Some(signature) = lookup(*definition)
                && let SignatureKind::Type(declaration) = &signature.kind
                && let Some(representation) = declaration.representation
                && let Ok(representation) = crate::graph::expand(
                    types,
                    representation,
                    arguments,
                    &signature.metadata.roles,
                )
                && let TypeKind::Variant(variants) = &types.row(representation).kind
            {
                for (variant, arm) in variants.iter().enumerate() {
                    parts.extend(arm.fields.iter().enumerate().map(|(field, (_, ty))| {
                        (
                            ResultStep::Payload {
                                variant: variant as u32,
                                field: field as u32,
                            },
                            *ty,
                        )
                    }));
                }
            }
        }
        _ => (),
    }
    parts
}

/// MODE-1: the disjoint parts of a declared result that select their own
/// relation, by path from the root, outermost first. A whole-result relation
/// selects the root (an empty path); every part outside a selected one is
/// global. Empty when the result is global throughout.
pub fn result_relations(
    types: &crate::TypeInterner,
    root: crate::TypeId,
    roles: &crate::NominalRoles,
    lookup: &mut impl FnMut(hirsel_syntax::DefId) -> Option<crate::SignatureRef>,
) -> Vec<(Vec<ResultStep>, crate::Relation)> {
    let mut selected = Vec::new();
    let mut pending = vec![(Vec::new(), root)];
    while let Some((path, ty)) = pending.pop() {
        let relation = &types.row(ty).modes.relation;
        if !relation.is_global() {
            selected.push((path, relation.clone()));
            continue;
        }
        for (step, part) in result_parts(types, ty, roles, lookup).into_iter().rev() {
            let mut path = path.clone();
            path.push(step);
            pending.push((path, part));
        }
    }
    selected
}

/// `root` as a caller holds it: its root relation stays, and every selected
/// part (MODE-1) is global. Part relations name the callee's parameters, so
/// the caller tracks them in flow, never in its types. Only carriers hold a
/// part (OWN-8), so tuple elements and type arguments are the only rows
/// rewritten; function types keep their own relations.
pub fn erase_result_parts(types: &crate::TypeInterner, root: crate::TypeId) -> crate::TypeId {
    erase_parts_below(types, root)
}

fn erase_parts_below(types: &crate::TypeInterner, ty: crate::TypeId) -> crate::TypeId {
    use crate::{GenericArgument, TypeKind};
    let part = |ty: crate::TypeId| {
        let row = types.row(ty);
        if row.modes.relation.is_global() {
            erase_parts_below(types, ty)
        } else {
            let mut row = row.clone();
            row.modes.relation = crate::Relation::global();
            erase_parts_below(types, types.intern(row))
        }
    };
    let row = types.row(ty);
    let kind = match &row.kind {
        TypeKind::Tuple(elements) => {
            let erased: Vec<_> = elements.iter().map(|e| part(*e)).collect();
            if erased.iter().eq(elements.iter()) {
                return ty;
            }
            TypeKind::Tuple(erased.into())
        }
        TypeKind::Named {
            definition,
            arguments,
        } => {
            let erased: Vec<_> = arguments
                .iter()
                .map(|a| match a {
                    GenericArgument::Type(t) => GenericArgument::Type(part(*t)),
                    other => other.clone(),
                })
                .collect();
            if erased.iter().eq(arguments.iter()) {
                return ty;
            }
            TypeKind::Named {
                definition: *definition,
                arguments: erased.into(),
            }
        }
        _ => return ty,
    };
    let mut row = row.clone();
    row.kind = kind;
    types.intern(row)
}

/// The relations a global result row's parts select (MODE-1): every
/// non-global relation below a global root, through tuple elements and type
/// arguments, not below a part that selects one.
pub fn part_relations(types: &crate::TypeInterner, result: crate::TypeId) -> Vec<crate::Relation> {
    let mut relations = vec![];
    if !types.get(result).unwrap().modes.relation.is_global() {
        return relations;
    }
    let mut pending = vec![result];
    let mut seen = std::collections::HashSet::new();
    while let Some(ty) = pending.pop() {
        if !seen.insert(ty) {
            continue;
        }
        let row = types.get(ty).unwrap();
        if ty != result && !row.modes.relation.is_global() {
            relations.push(row.modes.relation.clone());
            continue;
        }
        match &row.kind {
            crate::TypeKind::Tuple(elements) => pending.extend(elements.iter().copied()),
            crate::TypeKind::Named { arguments, .. } => {
                pending.extend(arguments.iter().filter_map(|a| match a {
                    crate::GenericArgument::Type(t) => Some(*t),
                    _ => None,
                }))
            }
            _ => {}
        }
    }
    relations
}
