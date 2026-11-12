//! Nominal representation expansion for stage-owned layout walks.
use hirsel_intern::work::{self, Counter};
use hirsel_syntax::DefId;
use hirsel_types::*;
use std::collections::{HashMap, HashSet};

/// Why a representation walk stopped before reaching a structural row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepresentationFailure {
    MissingType,
    MissingDeclaration,
    NotTypeDeclaration,
    MissingRepresentation,
    RecursiveRepresentation,
    Graph(GraphError),
    Query(QueryError),
}

/// The last row reached, plus the exact failure. A caller may retain that row
/// conservatively, but operational query failures must still propagate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepresentationError {
    pub root: TypeId,
    pub failure: RepresentationFailure,
}

/// Expand successive nominal representations in caller-owned storage.
/// `steps: Some(1)` performs one substitution; `None` reaches a structural row.
/// Zero steps returns the input without a declaration read. One phase-owned
/// callback supplies the declaration and actual argument authorities, so both
/// can use the same mutable query context without competing borrows. It receives
/// the scratch store rather than requiring a store clone. This preserves J's
/// evaluated authority and physical layout walks' conservative Unknown authority.
/// The guard spends its budget only on active polymorphic constructor growth.
pub fn expand_representation(
    store: &TypeInterner,
    memo: &TypeMemo,
    mut root: TypeId,
    lookup: &mut impl FnMut(
        &TypeInterner,
        DefId,
        &[GenericArgument],
    ) -> Result<Option<(SignatureRef, Vec<AuthoritySummary>)>, QueryError>,
    steps: Option<usize>,
) -> Result<TypeId, RepresentationError> {
    let mut expansions = NominalExpansions::default();
    let mut remaining = steps;
    loop {
        let error = |failure| RepresentationError { root, failure };
        let row = store
            .get(root)
            .ok_or_else(|| error(RepresentationFailure::MissingType))?;
        if remaining == Some(0) {
            return Ok(root);
        }
        let TypeKind::Named {
            definition,
            arguments,
        } = &row.kind
        else {
            return Ok(root);
        };
        work::count(Counter::ContentRelationRows);
        let definition = *definition;
        let arguments = arguments.clone();
        if !expansions
            .enter(root, 0, store)
            .map_err(|e| error(RepresentationFailure::Query(e)))?
        {
            return Err(error(RepresentationFailure::RecursiveRepresentation));
        }
        let (signature, authorities) = lookup(store, definition, &arguments)
            .map_err(|e| error(RepresentationFailure::Query(e)))?
            .ok_or_else(|| error(RepresentationFailure::MissingDeclaration))?;
        let SignatureKind::Type(declaration) = &signature.kind else {
            return Err(error(RepresentationFailure::NotTypeDeclaration));
        };
        let representation = declaration
            .representation
            .ok_or_else(|| error(RepresentationFailure::MissingRepresentation))?;
        root = substitute(
            store,
            memo,
            representation,
            &Substitution {
                binder: Some(GenericBinder::Bound(0)),
                arguments: &arguments,
                authorities: &authorities,
                roles: &signature.metadata.roles,
            },
        )
        .map_err(|e| error(RepresentationFailure::Graph(e)))?;
        remaining = remaining.map(|n| n - 1);
    }
}

/// MODE-1, PROV-4: whether storage of type `holder` could come to hold a part
/// of a value of type `content`, by type: some counted part of each has the
/// same shape. Nominal parts are read through their representations. A
/// generic, callable, erased or unexpandable part could be anything, and so
/// may hold anything; plain scalars hold no storage.
pub fn may_hold_content(
    store: &TypeInterner,
    memo: &TypeMemo,
    holder: TypeId,
    content: TypeId,
    lookup: &mut impl FnMut(DefId) -> Option<SignatureRef>,
) -> Result<bool, QueryError> {
    let (Some(holders), Some(contents)) = (
        counted_parts(store, memo, holder, lookup)?,
        counted_parts(store, memo, content, lookup)?,
    ) else {
        return Ok(true);
    };
    // A nominal only compares with the same declaration, and a structural
    // part only with the same constructor. Disjoint nominal sets therefore
    // cost one index insertion/lookup each, rather than all pairs.
    let mut shapes = Shapes::default();
    let mut index: HashMap<PartKey, ShapeBucket> = HashMap::new();
    for part in contents {
        work::count(Counter::ContentRelationRows);
        let bucket = index.entry(part_key(store, part)).or_default();
        bucket.all.push(part);
        if let Some(shape) = shapes.class(store, part) {
            bucket.exact.insert(shape);
        } else {
            bucket.unknown.push(part);
        }
    }
    let mut comparisons = HashMap::new();
    for holder in holders {
        work::count(Counter::ContentRelationRows);
        if let Some(bucket) = index.get(&part_key(store, holder)) {
            let contents = if let Some(shape) = shapes.class(store, holder) {
                if bucket.exact.contains(&shape) {
                    return Ok(true);
                }
                &bucket.unknown
            } else {
                &bucket.all
            };
            for content in contents {
                if same_shape(store, holder, *content, &mut comparisons) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[derive(Default)]
struct ShapeBucket {
    exact: HashSet<usize>,
    unknown: Vec<TypeId>,
    all: Vec<TypeId>,
}

#[derive(Eq, PartialEq, Hash)]
enum ShapeArgument {
    Type(usize),
    Size(Size),
}

#[derive(Eq, PartialEq, Hash)]
struct ShapeKey {
    constructor: PartKey,
    arguments: Vec<ShapeArgument>,
}

/// Hash-cons the relation's exact constructors, following graph.rs's finite
/// structural worklist. Labels and modes do not participate. A wildcard has
/// no exact class, so its pairs still go through same_shape, never a rejection.
#[derive(Default)]
struct Shapes {
    rows: HashMap<TypeId, Option<usize>>,
    classes: HashMap<ShapeKey, usize>,
}
impl Shapes {
    fn class(&mut self, store: &TypeInterner, root: TypeId) -> Option<usize> {
        work::count(Counter::ContentRelationRows);
        if let Some(class) = self.rows.get(&root) {
            return *class;
        }
        let mut pending = vec![(root, false)];
        let mut active = HashSet::new();
        while let Some((ty, exit)) = pending.pop() {
            work::count(Counter::ContentRelationRows);
            if self.rows.contains_key(&ty) {
                continue;
            }
            let Some(row) = store.get(ty) else {
                self.rows.insert(ty, None);
                continue;
            };
            if matches!(
                row.kind,
                TypeKind::Parameter(_) | TypeKind::Function(_) | TypeKind::Error
            ) {
                self.rows.insert(ty, None);
                continue;
            }
            if !exit {
                if !active.insert(ty) {
                    self.rows.insert(ty, None);
                    continue;
                }
                pending.push((ty, true));
                pending.extend(
                    row.kind
                        .value_parts()
                        .into_iter()
                        .map(|child| (child, false)),
                );
                continue;
            }
            active.remove(&ty);
            let arguments = match &row.kind {
                TypeKind::Named { arguments, .. } | TypeKind::Builtin { arguments, .. } => {
                    arguments
                        .iter()
                        .map(|argument| {
                            work::count(Counter::ContentRelationRows);
                            match argument {
                                GenericArgument::Type(child) => {
                                    self.rows[child].map(ShapeArgument::Type)
                                }
                                GenericArgument::Size(size) => {
                                    Some(ShapeArgument::Size(size.clone()))
                                }
                            }
                        })
                        .collect::<Option<Vec<_>>>()
                }
                kind => kind
                    .value_parts()
                    .iter()
                    .map(|child| {
                        work::count(Counter::ContentRelationRows);
                        self.rows[child].map(ShapeArgument::Type)
                    })
                    .collect(),
            };
            let class = arguments.map(|arguments| {
                let next = self.classes.len();
                *self
                    .classes
                    .entry(ShapeKey {
                        constructor: part_key(store, ty),
                        arguments,
                    })
                    .or_insert(next)
            });
            self.rows.insert(ty, class);
        }
        self.rows[&root]
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
enum PartKey {
    Primitive(Primitive),
    Named(DefId),
    Builtin(BuiltinType),
    Box,
    Tuple,
    Record,
    Variant,
}
fn part_key(store: &TypeInterner, ty: TypeId) -> PartKey {
    match &store.get(ty).expect("a counted part has a row").kind {
        TypeKind::Primitive(p) => PartKey::Primitive(*p),
        TypeKind::Named { definition, .. } => PartKey::Named(*definition),
        TypeKind::Builtin { kind, .. } => PartKey::Builtin(*kind),
        TypeKind::Box(_) => PartKey::Box,
        TypeKind::Tuple(_) => PartKey::Tuple,
        TypeKind::Record(_) => PartKey::Record,
        TypeKind::Variant(_) => PartKey::Variant,
        _ => unreachable!("unknown parts stop counted_parts"),
    }
}

/// Every counted part of `root`, nominal representations included, or None
/// when some part could be anything. A resource failure is never an unknown
/// part: the caller must propagate it before publishing an analysis result.
fn counted_parts(
    store: &TypeInterner,
    memo: &TypeMemo,
    root: TypeId,
    lookup: &mut impl FnMut(DefId) -> Option<SignatureRef>,
) -> Result<Option<Vec<TypeId>>, QueryError> {
    let mut parts = vec![];
    let mut pending = vec![(root, false)];
    let mut seen = HashSet::new();
    let mut expansions = NominalExpansions::default();
    while let Some((ty, exit)) = pending.pop() {
        if exit {
            expansions.leave(ty, 0);
            continue;
        }
        if !seen.insert(ty) {
            continue;
        }
        work::count(Counter::ContentRelationRows);
        let Some(row) = store.get(ty) else {
            return Ok(None);
        };
        match &row.kind {
            TypeKind::Primitive(Primitive::String | Primitive::Bytes) => parts.push(ty),
            TypeKind::Primitive(_) => {}
            TypeKind::Parameter(_)
            | TypeKind::Function(_)
            | TypeKind::Error
            | TypeKind::Builtin {
                kind: BuiltinType::Iter,
                ..
            } => return Ok(None),
            TypeKind::Named { definition, .. } => {
                let Some(signature) = lookup(*definition) else {
                    return Ok(None);
                };
                let SignatureKind::Type(declaration) = &signature.kind else {
                    return Ok(None);
                };
                // A checked public Plain bound proves there is no counted
                // storage, including an opaque export such as std's Date.
                if declaration.bounds.contains(&Bound::Plain) {
                    continue;
                }
                parts.push(ty);
                if !expansions.enter(ty, 0, store)? {
                    continue;
                }
                pending.push((ty, true));
                let representation = match expand_representation(
                    store,
                    memo,
                    ty,
                    &mut |_, path, arguments| {
                        Ok(lookup(path).map(|signature| {
                            (signature, vec![AuthoritySummary::Unknown; arguments.len()])
                        }))
                    },
                    None,
                ) {
                    Ok(ty) => ty,
                    Err(RepresentationError {
                        failure: RepresentationFailure::Query(error),
                        ..
                    }) => return Err(error),
                    Err(_) => return Ok(None),
                };
                pending.push((representation, false));
            }
            kind => {
                parts.push(ty);
                pending.extend(kind.value_parts().into_iter().map(|ty| (ty, false)));
            }
        }
    }
    Ok(Some(parts))
}

/// The storage-shape relation, ignoring modes, brands, relations and field
/// labels. Unknown, generic and callable rows match any shape, as before.
/// The finite pair worklist follows graph.rs's structural comparison, with
/// this relation's own constructor and wildcard rules.
fn same_shape(
    store: &TypeInterner,
    a: TypeId,
    b: TypeId,
    memo: &mut HashMap<(TypeId, TypeId), bool>,
) -> bool {
    let mut pending = vec![(a, b)];
    let mut seen = HashSet::new();
    let mut parents: HashMap<_, Vec<_>> = HashMap::new();
    while let Some((p, q)) = pending.pop() {
        work::count(Counter::ContentRelationRows);
        if p == q || !seen.insert((p, q)) {
            continue;
        }
        if let Some(equal) = memo.get(&(p, q)) {
            if *equal {
                continue;
            }
            reject_shape_pair((p, q), memo, &parents);
            return false;
        }
        let (Some(x), Some(y)) = (store.get(p), store.get(q)) else {
            continue;
        };
        let mut children = vec![];
        let mut arguments = |p: &[GenericArgument], q: &[GenericArgument]| {
            p.len() == q.len()
                && p.iter().zip(q).all(|pair| match pair {
                    (GenericArgument::Type(p), GenericArgument::Type(q)) => {
                        children.push((*p, *q));
                        true
                    }
                    (p, q) => p == q,
                })
        };
        let equal = match (&x.kind, &y.kind) {
            (TypeKind::Parameter(_) | TypeKind::Function(_) | TypeKind::Error, _)
            | (_, TypeKind::Parameter(_) | TypeKind::Function(_) | TypeKind::Error) => true,
            (TypeKind::Primitive(p), TypeKind::Primitive(q)) => p == q,
            (
                TypeKind::Builtin {
                    kind: p,
                    arguments: a,
                },
                TypeKind::Builtin {
                    kind: q,
                    arguments: b,
                },
            ) => p == q && arguments(a, b),
            (
                TypeKind::Named {
                    definition: p,
                    arguments: a,
                },
                TypeKind::Named {
                    definition: q,
                    arguments: b,
                },
            ) => p == q && arguments(a, b),
            (TypeKind::Box(p), TypeKind::Box(q)) => {
                children.push((*p, *q));
                true
            }
            (TypeKind::Tuple(_) | TypeKind::Record(_) | TypeKind::Variant(_), _) => {
                let p = x.kind.value_parts();
                let q = y.kind.value_parts();
                let equal = std::mem::discriminant(&x.kind) == std::mem::discriminant(&y.kind)
                    && p.len() == q.len();
                if equal {
                    children.extend(p.into_iter().zip(q));
                }
                equal
            }
            _ => false,
        };
        if !equal {
            reject_shape_pair((p, q), memo, &parents);
            return false;
        }
        for child in children {
            work::count(Counter::ContentRelationRows);
            parents.entry(child).or_default().push((p, q));
            pending.push(child);
        }
    }
    for pair in seen {
        memo.insert(pair, true);
    }
    true
}

fn reject_shape_pair(
    failed: (TypeId, TypeId),
    memo: &mut HashMap<(TypeId, TypeId), bool>,
    parents: &HashMap<(TypeId, TypeId), Vec<(TypeId, TypeId)>>,
) {
    let mut pending = vec![failed];
    let mut seen = HashSet::new();
    while let Some(pair) = pending.pop() {
        work::count(Counter::ContentRelationRows);
        if seen.insert(pair) {
            memo.insert(pair, false);
            pending.extend(parents.get(&pair).into_iter().flatten().copied());
        }
    }
}
