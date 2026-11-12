//! Logical sizes (MEM-6): the target-independent bytes a bounded arena charges for a
//! value, read from type structure alone, never from a physical layout.
//!
//! A scalar is its width, a reference to a block (`box T`, `String`, `Bytes`, `List`,
//! `Array`, `Map`, `Set`, `SortedMap`) or an opaque nominal (a capability, handle or
//! arena token) is 8, a callable or `Iter` is 16, a tuple, record or resource is the sum
//! of its fields, a variant is 4 plus its largest variant's fields, and `Inline[T, n]`
//! and `Simd[T, n]` are `n` times `T`. Padding, headers and slabs are never charged.
use hirsel_syntax::{DefId, DefPath};
use hirsel_types::{
    BinderRef, BuiltinType, GenericArgument, Primitive, Size, Type, TypeId, TypeInterner, TypeKind,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// What a nominal type contributes: its representation's shape, whose
/// `Bound { depth: 0 }` parameters are the nominal's generic actuals, or an
/// opaque word.
#[derive(Clone, Copy, Debug)]
pub enum LogicalNominal {
    Representation { shape: TypeId },
    Opaque,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalError {
    /// A type or size parameter with no actual: a generic body running parametrically.
    Parameter,
    /// A nominal type the resolver does not know.
    Unresolved(DefPath),
    /// A missing row, an invalid actual, or an infinitely recursive inline representation.
    Malformed,
}

/// The tag a variant is charged besides its largest variant's fields.
pub const VARIANT_TAG: u64 = 4;
/// A block reference, an opaque nominal or an arena token.
pub const REFERENCE: u64 = 8;
/// A callable (code and environment) or an erased `Iter`.
pub const CALLABLE: u64 = 16;

/// The store of a logical shape. Nominal actuals are instantiated into the
/// same interner before returning a structural row, so children keep their
/// exact context without a recursively owned chain of scope frames.
#[derive(Clone)]
pub struct Scope<'a>(&'a TypeInterner);

impl<'a> Scope<'a> {
    pub fn root(store: &'a TypeInterner) -> Self {
        Self(store)
    }
    pub fn store(&self) -> &'a TypeInterner {
        self.0
    }
}

/// A structural row reached through parameters and nominal representations,
/// with the scope its child types belong to.
pub enum Resolved<'a> {
    Structural(&'a Type, Scope<'a>),
    Opaque,
}

type Instances = HashMap<(usize, TypeId, Vec<GenericArgument>), TypeId>;

/// Logical sizes and resolved structure under one nominal resolver.
pub struct Logical<'a, 'r> {
    nominal: &'r dyn Fn(DefId) -> Option<LogicalNominal>,
    /// Rows read through scopes of this lifetime.
    rows: std::marker::PhantomData<&'a TypeInterner>,
    instances: RefCell<Instances>,
}

impl<'a, 'r> Logical<'a, 'r> {
    pub fn new(nominal: &'r dyn Fn(DefId) -> Option<LogicalNominal>) -> Self {
        Self {
            nominal,
            rows: std::marker::PhantomData,
            instances: RefCell::new(HashMap::new()),
        }
    }

    /// Follow nominals to their representations with concrete actuals.
    pub fn resolve(&self, mut ty: TypeId, scope: &Scope<'a>) -> Result<Resolved<'a>, LogicalError> {
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(ty) {
                return Err(LogicalError::Malformed);
            }
            let row = scope.store().get(ty).ok_or(LogicalError::Malformed)?;
            match &row.kind {
                TypeKind::Parameter(_) => return Err(LogicalError::Parameter),
                TypeKind::Named {
                    definition,
                    arguments,
                } => match (self.nominal)(*definition)
                    .ok_or_else(|| LogicalError::Unresolved(definition.path().clone()))?
                {
                    LogicalNominal::Opaque => return Ok(Resolved::Opaque),
                    LogicalNominal::Representation { shape } => {
                        ty = self.instantiate(scope.store(), shape, arguments)?;
                    }
                },
                TypeKind::Error => return Err(LogicalError::Malformed),
                _ => return Ok(Resolved::Structural(row, scope.clone())),
            }
        }
    }

    /// Instantiate only logical value structure. Callables are opaque words;
    /// their headers have no logical children. Like the generic transport's
    /// postorder graph walks, this memo visits each template row once, and
    /// never rewrites a supplied actual in the declaration's binder.
    fn instantiate(
        &self,
        store: &TypeInterner,
        root: TypeId,
        arguments: &[GenericArgument],
    ) -> Result<TypeId, LogicalError> {
        let key = (std::ptr::from_ref(store) as usize, root, arguments.to_vec());
        if let Some(id) = self.instances.borrow().get(&key) {
            return Ok(*id);
        }
        let mut rows = HashMap::new();
        let mut active = HashSet::new();
        let mut pending = vec![(root, false)];
        while let Some((id, exit)) = pending.pop() {
            if rows.contains_key(&id) {
                continue;
            }
            let row = store.get(id).ok_or(LogicalError::Malformed)?;
            if let TypeKind::Parameter(BinderRef::Bound {
                depth: 0,
                parameter,
            }) = &row.kind
            {
                let Some(GenericArgument::Type(actual)) = arguments.get(parameter.0 as usize)
                else {
                    return Err(LogicalError::Malformed);
                };
                rows.insert(id, *actual);
                continue;
            }
            if !exit {
                if !active.insert(id) {
                    return Err(LogicalError::Malformed);
                }
                pending.push((id, true));
                pending.extend(
                    row.kind
                        .value_parts()
                        .into_iter()
                        .map(|child| (child, false)),
                );
                continue;
            }
            let mut instantiated = row.clone();
            if !matches!(row.kind, TypeKind::Function(_)) {
                instantiated.kind = row.kind.map_edges(|child| rows[&child]);
            }
            if let TypeKind::Named {
                arguments: actuals, ..
            }
            | TypeKind::Builtin {
                arguments: actuals, ..
            } = &mut instantiated.kind
            {
                for actual in actuals.iter_mut() {
                    if let GenericArgument::Size(Size::Parameter(BinderRef::Bound {
                        depth: 0,
                        parameter,
                    })) = actual
                    {
                        let Some(GenericArgument::Size(size)) = arguments.get(parameter.0 as usize)
                        else {
                            return Err(LogicalError::Malformed);
                        };
                        *actual = GenericArgument::Size(size.clone());
                    }
                }
            }
            rows.insert(id, store.intern(instantiated));
            active.remove(&id);
        }
        let id = rows[&root];
        self.instances.borrow_mut().insert(key, id);
        Ok(id)
    }

    /// A generic size argument's value. Resolved structural rows already
    /// contain their concrete nominal actuals.
    pub fn count(&self, size: &Size, _scope: &Scope<'a>) -> Result<u64, LogicalError> {
        match size {
            Size::Constant(n) => Ok(*n),
            Size::Parameter(_) => Err(LogicalError::Parameter),
        }
    }

    /// The logical size of a value of `ty`.
    pub fn size(&self, ty: TypeId, scope: &Scope<'a>) -> Result<u64, LogicalError> {
        self.checked_size(ty, scope)
            .map(|size| size.unwrap_or(u64::MAX))
    }

    /// An exact logical size. None means arithmetic overflow, which exceeds
    /// even a u64::MAX arena capacity. Legacy saturating size queries retain
    /// their existing result through [`Self::size`].
    pub fn checked_size(&self, ty: TypeId, scope: &Scope<'a>) -> Result<Option<u64>, LogicalError> {
        self.size_at(ty, scope)
    }

    /// The logical element size of a `List`, `Array`, `Map` (key plus value) or
    /// `Set` block type, the unit of its capacity charge. None for any other type.
    pub fn element(&self, ty: TypeId, scope: &Scope<'a>) -> Result<Option<u64>, LogicalError> {
        let Resolved::Structural(row, scope) = self.resolve(ty, scope)? else {
            return Ok(None);
        };
        let TypeKind::Builtin { kind, arguments } = &row.kind else {
            return Ok(None);
        };
        let types = type_arguments(arguments);
        let parts: &[TypeId] = match (kind, types.as_slice()) {
            (BuiltinType::List | BuiltinType::Array | BuiltinType::Set, [element, ..]) => {
                std::slice::from_ref(element)
            }
            (BuiltinType::Map | BuiltinType::SortedMap, [_, _, ..]) => &types[..2],
            _ => return Ok(None),
        };
        let mut total = 0u64;
        for part in parts {
            total = total.saturating_add(self.size(*part, &scope)?);
        }
        Ok(Some(total))
    }

    fn size_at(&self, root: TypeId, scope: &Scope<'a>) -> Result<Option<u64>, LogicalError> {
        let mut sizes = HashMap::new();
        let mut active = HashSet::new();
        let mut pending = vec![(root, false)];
        while let Some((ty, exit)) = pending.pop() {
            if sizes.contains_key(&ty) {
                continue;
            }
            let Resolved::Structural(row, scope) = self.resolve(ty, scope)? else {
                sizes.insert(ty, Some(REFERENCE));
                continue;
            };
            let parts = match &row.kind {
                TypeKind::Tuple(_) | TypeKind::Record(_) | TypeKind::Variant(_) => {
                    row.kind.value_parts()
                }
                TypeKind::Builtin {
                    kind: BuiltinType::Inline | BuiltinType::Simd | BuiltinType::Packed,
                    arguments,
                } => type_arguments(arguments).into_iter().take(1).collect(),
                _ => vec![],
            };
            if !exit {
                if !active.insert(ty) {
                    return Err(LogicalError::Malformed);
                }
                pending.push((ty, true));
                pending.extend(parts.into_iter().map(|part| (part, false)));
                continue;
            }
            let sum = |parts: &[TypeId]| {
                parts.iter().fold(Some(0u64), |total, part| {
                    total.zip(sizes[part]).and_then(|(a, b)| a.checked_add(b))
                })
            };
            let size = match &row.kind {
                TypeKind::Primitive(p) => Some(primitive(*p)),
                TypeKind::Tuple(_) | TypeKind::Record(_) => sum(&parts),
                TypeKind::Variant(variants) => {
                    let largest = variants.iter().fold(Some(0u64), |largest, variant| {
                        let fields: Vec<_> = variant.fields.iter().map(|(_, ty)| *ty).collect();
                        largest.zip(sum(&fields)).map(|(a, b)| a.max(b))
                    });
                    largest.and_then(|n| VARIANT_TAG.checked_add(n))
                }
                TypeKind::Function(_) => Some(CALLABLE),
                TypeKind::Box(_) => Some(REFERENCE),
                TypeKind::Builtin { kind, arguments } => {
                    let element = type_arguments(arguments).first().copied();
                    let count = arguments.iter().find_map(|a| match a {
                        GenericArgument::Size(s) => Some(s),
                        _ => None,
                    });
                    match (kind, element, count) {
                        (BuiltinType::Inline | BuiltinType::Simd, Some(element), Some(count)) => {
                            let count = self.count(count, &scope)?;
                            if count == 0 {
                                Some(0)
                            } else {
                                sizes[&element].and_then(|n| count.checked_mul(n))
                            }
                        }
                        (BuiltinType::Packed, Some(element), _) => sizes[&element],
                        (BuiltinType::Iter, _, _) => Some(CALLABLE),
                        _ => Some(REFERENCE),
                    }
                }
                TypeKind::Named { .. } | TypeKind::Parameter(_) | TypeKind::Error => {
                    return Err(LogicalError::Malformed);
                }
            };
            sizes.insert(ty, size);
            active.remove(&ty);
        }
        Ok(sizes[&root])
    }
}

/// The type actuals of a builtin, in order.
pub fn type_arguments(arguments: &[GenericArgument]) -> Vec<TypeId> {
    arguments
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(t) => Some(*t),
            GenericArgument::Size(_) => None,
        })
        .collect()
}

/// A primitive's logical width.
pub fn primitive(p: Primitive) -> u64 {
    match p {
        Primitive::Never => 0,
        Primitive::Bool | Primitive::U8 | Primitive::I8 => 1,
        Primitive::U16 | Primitive::I16 => 2,
        Primitive::U32 | Primitive::I32 | Primitive::F32 | Primitive::Char => 4,
        Primitive::U64 | Primitive::I64 | Primitive::F64 => 8,
        Primitive::U128 | Primitive::I128 => 16,
        Primitive::String | Primitive::Bytes => REFERENCE,
    }
}

/// The logical size of `ty` in `store`.
pub fn logical_size(
    store: &TypeInterner,
    ty: TypeId,
    nominal: &dyn Fn(DefId) -> Option<LogicalNominal>,
) -> Result<u64, LogicalError> {
    Logical::new(nominal).size(ty, &Scope::root(store))
}

/// The logical element size of a block type in `store` (see [`Logical::element`]).
pub fn logical_element(
    store: &TypeInterner,
    ty: TypeId,
    nominal: &dyn Fn(DefId) -> Option<LogicalNominal>,
) -> Result<Option<u64>, LogicalError> {
    Logical::new(nominal).element(ty, &Scope::root(store))
}
