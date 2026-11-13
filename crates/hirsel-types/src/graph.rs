//! Pure graph transport. Signature lookup and rule diagnostics remain phase-owned.
use crate::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
type IdMap<K, V> = std::collections::HashMap<K, V, hirsel_intern::IdHasher>;
type IdSet<K> = std::collections::HashSet<K, hirsel_intern::IdHasher>;

/// Role of an edge in the complete structural type graph. Callable edges cross
/// one binder for value, type and size parameters, even on nongeneric callables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeEdgeRole {
    Component,
    TypeArgument,
    FunctionParameter,
    FunctionResult,
    FunctionError,
}
impl TypeEdgeRole {
    pub fn parameter_binders(self) -> u32 {
        match self {
            Self::Component | Self::TypeArgument => 0,
            Self::FunctionParameter | Self::FunctionResult | Self::FunctionError => 1,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeGraphEdge {
    pub ty: TypeId,
    pub role: TypeEdgeRole,
}
impl TypeKind {
    /// Structural value parts in declaration order, excluding callable headers.
    /// Nominal/builtin type arguments are included even when phantom; consumers
    /// retain their own owned-payload, declaration and cycle policies.
    pub fn value_parts(&self) -> Vec<TypeId> {
        self.graph_edges()
            .into_iter()
            .filter(|e| e.role.parameter_binders() == 0)
            .map(|e| e.ty)
            .collect()
    }
    /// SEM-5 entry layout in declaration order: read-only key, then value.
    /// Consumers retain their own projection, use-mode and bound policies.
    pub fn map_entry_fields(&self) -> Option<[TypeId; 2]> {
        match self {
            Self::Builtin {
                kind: BuiltinType::Map | BuiltinType::SortedMap,
                arguments,
            } => match arguments.as_slice() {
                [GenericArgument::Type(key), GenericArgument::Type(value)] => Some([*key, *value]),
                _ => None,
            },
            _ => None,
        }
    }
    /// Complete graph view, in declaration order, including all callable edges.
    /// This does not describe owned payloads or separately executed closure bodies.
    /// Callers retain their own cycle, provenance and owned-payload traversal policies.
    pub fn graph_edges(&self) -> Vec<TypeGraphEdge> {
        let mut count = 0;
        self.visit_graph_edges(|_| count += 1);
        let mut edges = Vec::with_capacity(count);
        self.visit_graph_edges(|edge| edges.push(edge));
        edges
    }
    pub fn visit_graph_edges(&self, mut visit: impl FnMut(TypeGraphEdge)) {
        use TypeEdgeRole as R;
        let mut edge = |ty, role| visit(TypeGraphEdge { ty, role });
        match self {
            Self::Tuple(ts) => ts.iter().for_each(|t| edge(*t, R::Component)),
            Self::Box(t) => edge(*t, R::Component),
            Self::Record(fs) => fs.iter().for_each(|f| edge(f.ty, R::Component)),
            Self::Variant(vs) => {
                for variant in vs {
                    for field in &variant.fields {
                        edge(field.1, R::Component);
                    }
                }
            }
            Self::Named { arguments, .. } | Self::Builtin { arguments, .. } => {
                arguments.iter().for_each(|a| {
                    if let GenericArgument::Type(t) = a {
                        edge(*t, R::TypeArgument);
                    }
                })
            }
            Self::Function(f) => {
                for parameter in &f.parameters {
                    edge(parameter.ty, R::FunctionParameter);
                }
                edge(f.result, R::FunctionResult);
                if let Some(error) = f.error {
                    edge(error, R::FunctionError);
                }
            }
            Self::Primitive(_) | Self::Parameter(_) | Self::Error => {}
        }
    }
}
/// Whether `ty` names a generic type or size parameter anywhere in its graph.
pub fn mentions_parameter(store: &TypeInterner, ty: TypeId) -> bool {
    let mut seen = IdSet::default();
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        if !seen.insert(ty) {
            continue;
        }
        let Some(row) = store.get(ty) else {
            return true;
        };
        let sized = match &row.kind {
            TypeKind::Parameter(_) => return true,
            TypeKind::Named { arguments, .. } | TypeKind::Builtin { arguments, .. } => arguments
                .iter()
                .any(|a| matches!(a, GenericArgument::Size(Size::Parameter(_)))),
            _ => false,
        };
        if sized {
            return true;
        }
        pending.extend(row.kind.graph_edges().into_iter().map(|e| e.ty));
    }
    false
}
/// The closed generic arguments of an invocation, rows of the session interner.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Actuals {
    pub arguments: Vec<GenericArgument>,
}
/// The actuals bound to each enclosing generic binder of an invocation.
pub type ActualBindings = BTreeMap<SiteRef, Actuals>;
/// `arguments` with every parameter of a binder in `bindings` replaced by that
/// binder's actuals.
pub fn close_arguments(
    t: &TypeInterner,
    m: &TypeMemo,
    arguments: &[GenericArgument],
    bindings: &ActualBindings,
) -> Result<Actuals, GraphError> {
    let mut result = arguments.to_vec();
    for (&scope, actuals) in bindings.iter().filter(|_| !arguments.is_empty()) {
        let authorities = vec![AuthoritySummary::Unknown; actuals.arguments.len()];
        let substitution = Substitution {
            binder: Some(GenericBinder::Body(scope)),
            arguments: &actuals.arguments,
            authorities: &authorities,
            ..Default::default()
        };
        for argument in &mut result {
            match argument {
                GenericArgument::Type(ty) => *ty = substitute(t, m, *ty, &substitution)?,
                GenericArgument::Size(Size::Parameter(SizeParameterRef::Body {
                    scope: owner,
                    parameter,
                })) if *owner == scope => {
                    *argument = actuals
                        .arguments
                        .get(parameter.0 as usize)
                        .cloned()
                        .ok_or(GraphError::MissingArgument(parameter.0))?;
                }
                _ => (),
            }
        }
    }
    Ok(Actuals { arguments: result })
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphError {
    UnboundGeneric(GenericBinder),
    UnboundOrigin(OriginRoot),
    UnresolvedPlacement(TypeId),
    UnboundParameter(ParameterRef),
    MissingType(TypeId),
    StructuralCycle(TypeId),
    RecoveryType(TypeId),
    ArgumentKind(u32),
    MissingArgument(u32),
    MissingAuthority(u32),
}
/// Arguments belong to the destination store and replace the root declaration's
/// references. Nested callables retain their own references; enclosing references
/// cross the removed binder with relative depths adjusted.
/// Authority entries are required only when a replaced parameter occurs in an
/// authority summary; the caller obtains those summaries from declared signatures.
#[derive(Clone, Copy)]
pub struct Substitution<'a> {
    pub binder: Option<GenericBinder>,
    pub arguments: &'a [GenericArgument],
    pub authorities: &'a [AuthoritySummary],
    /// Nominal Result identity used to normalize derived callback error channels.
    pub roles: &'a NominalRoles,
}
impl Default for Substitution<'_> {
    fn default() -> Self {
        static ROLES: NominalRoles = NominalRoles {
            unsafe_capability: None,
            result_type: None,
            tasks_capability: None,
            scope_capability: None,
            heap: None,
        };
        Self {
            binder: None,
            arguments: &[],
            authorities: &[],
            roles: &ROLES,
        }
    }
}
/// The derived error channel is selected by nominal role, never by spelling.
pub fn result_error(store: &TypeInterner, result: TypeId, roles: &NominalRoles) -> Option<TypeId> {
    match &store.get(result)?.kind {
        TypeKind::Named {
            definition,
            arguments,
        } if roles.result_type.as_ref() == Some(definition.path()) => match arguments.as_slice() {
            [GenericArgument::Type(_), GenericArgument::Type(error)] => Some(*error),
            _ => None,
        },
        _ => None,
    }
}
/// Remove source value access modes from a generic type argument. Complete
/// actual rows stay in CheckedBody.calls; capture multiplicity/sendability remain
/// intrinsic to a callable type. Nested type arguments keep their own contracts.
/// `arena` says which relation places name arenas: a type argument transports
/// its actual's placement (PROV-4) but none of its lifetime.
pub fn generic_type_argument(
    store: &TypeInterner,
    actual: TypeId,
    arena: impl Fn(&Origin) -> bool,
) -> Result<TypeId, GraphError> {
    let mut row = store
        .get(actual)
        .cloned()
        .ok_or(GraphError::MissingType(actual))?;
    // Root access belongs to the actual value, including when that value has
    // a symbolic type. Arena places and regions describe content and stay.
    // A forwarded abstract parameter carries its enclosing declaration's
    // relation contract, independently of concrete holder inference.
    if !matches!(row.kind, TypeKind::Parameter(_))
        || row.modes.relation.named_parameters().is_some()
        || row.sendability_requirement == SendabilityRequirement::Sendable
    {
        row.modes.relation = row.modes.relation.retain(|r| match r {
            Reach::Place(o) => arena(o),
            Reach::Region(_) => true,
            Reach::Environment => false,
        });
    }
    row.modes.uniqueness = Uniqueness::Aliased;
    row.sendability_requirement = SendabilityRequirement::None;
    // An inferred type actual describes representation and receiving modes,
    // not the current operand proof. Each argument proves crossing separately.
    if !matches!(
        row.kind,
        TypeKind::Function(_)
            | TypeKind::Builtin {
                kind: BuiltinType::Iter,
                ..
            }
    ) {
        row.modes.sendability = Sendability::NotSendable;
    }
    Ok(store.intern(row))
}
/// Binder-aware substitution over the session interner. Source rows are
/// immutable and the result is interned beside them; nested callables keep
/// their own binders. Memoized session-wide in `m` by every explicit input:
/// the root, the binder, the arguments, their authorities and the roles.
pub fn substitute(
    t: &TypeInterner,
    m: &TypeMemo,
    ty: TypeId,
    s: &Substitution<'_>,
) -> Result<TypeId, GraphError> {
    // rustc type_ir/flags.rs:5-23: no generic transport for closed graphs.
    if !crate::parameters::needs_substitution(t, ty, s.roles.result_type.is_some()) {
        return Ok(ty);
    }
    m.substitute(ty, s, || {
        Importer {
            t,
            substitution: *s,
            memo: IdMap::default(),
            active: IdSet::default(),
        }
        .ty(ty, 0)
    })
}

/// A signature header's edges substituted at their own binder: every
/// parameter, result and error type, and the header's authority terms.
pub(crate) fn substitute_header(
    t: &TypeInterner,
    m: &TypeMemo,
    f: &FunctionType,
    s: &Substitution<'_>,
) -> Result<FunctionType, GraphError> {
    let mut f = f.clone();
    for p in f.parameters.iter_mut() {
        p.ty = substitute(t, m, p.ty, s)?;
    }
    f.result = substitute(t, m, f.result, s)?;
    f.error = f.error.map(|e| substitute(t, m, e, s)).transpose()?;
    if s.roles.result_type.is_some() {
        f.error = result_error(t, f.result, s.roles);
    }
    let importer = Importer {
        t,
        substitution: *s,
        memo: IdMap::default(),
        active: IdSet::default(),
    };
    importer.authority(&mut f.authority, 0)?;
    if let Uses::Only(a) = &mut f.uses {
        importer.authority(a, 0)?;
    }
    Ok(f)
}

/// A nominal's representation at its actual arguments, for proofs that open
/// declarations. Unmemoized: proofs own no structural memo.
pub(crate) fn expand(
    t: &TypeInterner,
    representation: TypeId,
    arguments: &[GenericArgument],
    roles: &NominalRoles,
) -> Result<TypeId, GraphError> {
    if !crate::parameters::needs_substitution(t, representation, roles.result_type.is_some()) {
        return Ok(representation);
    }
    let authorities = vec![AuthoritySummary::Unknown; arguments.len()];
    Importer {
        t,
        substitution: Substitution {
            binder: Some(GenericBinder::Bound(0)),
            arguments,
            authorities: &authorities,
            roles,
        },
        memo: IdMap::default(),
        active: IdSet::default(),
    }
    .ty(representation, 0)
}

struct Importer<'a> {
    t: &'a TypeInterner,
    substitution: Substitution<'a>,
    memo: IdMap<(TypeId, u32), TypeId>,
    active: IdSet<TypeId>,
}
fn raise_reference<P: Copy>(p: BinderRef<P>, amount: u32) -> BinderRef<P> {
    match p {
        BinderRef::Bound { depth, parameter } => BinderRef::Bound {
            depth: depth + amount,
            parameter,
        },
        p => p,
    }
}
impl Importer<'_> {
    fn reference<P: Copy>(&self, p: BinderRef<P>, nested: u32) -> (BinderRef<P>, bool) {
        let Some(target) = self.substitution.binder else {
            return (p, false);
        };
        match (p, target) {
            (BinderRef::Bound { depth, .. }, GenericBinder::Bound(target))
                if depth == target + nested =>
            {
                (p, true)
            }
            (BinderRef::Bound { depth, parameter }, GenericBinder::Bound(target))
                if depth > target + nested =>
            {
                (
                    BinderRef::Bound {
                        depth: depth - 1,
                        parameter,
                    },
                    false,
                )
            }
            (BinderRef::Body { scope, .. }, GenericBinder::Body(target)) if scope == target => {
                (p, true)
            }
            _ => (p, false),
        }
    }
    fn argument(&self, p: u32) -> Result<&GenericArgument, GraphError> {
        self.substitution
            .arguments
            .get(p as usize)
            .ok_or(GraphError::MissingArgument(p))
    }
    fn type_argument(&self, p: TypeParameterRef) -> Result<TypeId, GraphError> {
        match self.argument(p.parameter().0)? {
            GenericArgument::Type(t) => Ok(*t),
            _ => Err(GraphError::ArgumentKind(p.parameter().0)),
        }
    }
    fn drop_effect(
        &self,
        effect: DropAllocation,
        nested: u32,
    ) -> Result<DropAllocation, GraphError> {
        match effect {
            DropAllocation::DependsOn(ps) => {
                let mut effect = DropAllocation::NoAlloc;
                for p in ps {
                    let (p, selected) = self.reference(p, nested);
                    if !selected {
                        effect = effect.join(DropAllocation::DependsOn(vec![p].into()));
                        continue;
                    }
                    let id = self.type_argument(p)?;
                    effect = effect.join(
                        match self
                            .t
                            .get(id)
                            .ok_or(GraphError::MissingType(id))?
                            .drop_allocation
                            .clone()
                        {
                            DropAllocation::DependsOn(ps) => DropAllocation::DependsOn(
                                ps.into_iter().map(|p| raise_reference(p, nested)).collect(),
                            ),
                            effect => effect,
                        },
                    );
                }
                Ok(effect)
            }
            effect => Ok(effect),
        }
    }
    fn sendability(&self, effect: Sendability, nested: u32) -> Result<Sendability, GraphError> {
        match effect {
            Sendability::DependsOn(ps) => {
                let mut depends = BTreeSet::new();
                for p in ps {
                    let (p, selected) = self.reference(p, nested);
                    if !selected {
                        depends.insert(p);
                        continue;
                    }
                    let id = self.type_argument(p)?;
                    match &self
                        .t
                        .get(id)
                        .ok_or(GraphError::MissingType(id))?
                        .modes
                        .sendability
                    {
                        Sendability::NotSendable => return Ok(Sendability::NotSendable),
                        Sendability::Sendable => (),
                        Sendability::DependsOn(ps) => {
                            depends.extend(ps.iter().map(|p| raise_reference(*p, nested)))
                        }
                    }
                }
                Ok(if depends.is_empty() {
                    Sendability::Sendable
                } else {
                    Sendability::DependsOn(depends.into_iter().collect())
                })
            }
            effect => Ok(effect),
        }
    }
    fn authority(&self, summary: &mut AuthoritySummary, nested: u32) -> Result<(), GraphError> {
        let AuthoritySummary::Known(terms) = summary else {
            return Ok(());
        };
        let mut result = BTreeSet::new();
        for term in terms.iter() {
            if let AuthorityTerm::TypeParameter(p) = term {
                let (p, selected) = self.reference(*p, nested);
                if !selected {
                    result.insert(AuthorityTerm::TypeParameter(p));
                    continue;
                }
                self.type_argument(p)?;
                match self
                    .substitution
                    .authorities
                    .get(p.parameter().0 as usize)
                    .ok_or(GraphError::MissingAuthority(p.parameter().0))?
                {
                    AuthoritySummary::Unknown => {
                        *summary = AuthoritySummary::Unknown;
                        return Ok(());
                    }
                    AuthoritySummary::Known(terms) => {
                        result.extend(terms.iter().map(|term| match term {
                            AuthorityTerm::TypeParameter(p) => {
                                AuthorityTerm::TypeParameter(raise_reference(*p, nested))
                            }
                            AuthorityTerm::Parameter(p) => {
                                AuthorityTerm::Parameter(raise_reference(*p, nested))
                            }
                            term => term.clone(),
                        }))
                    }
                }
            } else {
                result.insert(term.clone());
            }
        }
        *summary = AuthoritySummary::Known(result.into_iter().collect());
        Ok(())
    }
    fn ty(&mut self, id: TypeId, nested: u32) -> Result<TypeId, GraphError> {
        // Skip only structurally closed subgraphs. Role-bearing callable
        // error channels and all generic/authority normalization remain live.
        if !crate::parameters::needs_substitution(
            self.t,
            id,
            self.substitution.roles.result_type.is_some(),
        ) {
            return Ok(id);
        }
        let key = (id, nested);
        if let Some(id) = self.memo.get(&key) {
            return Ok(*id);
        }
        if !self.active.insert(id) {
            return Err(GraphError::StructuralCycle(id));
        }
        let mut t = self.t.get(id).cloned().ok_or(GraphError::MissingType(id))?;
        if let TypeKind::Parameter(p) = t.kind {
            let (p, selected) = self.reference(p, nested);
            t.kind = TypeKind::Parameter(p);
            if selected {
                let target = self.type_argument(p)?;
                let target = shift_type_binders(self.t, target, nested)?;
                let mut replacement = self
                    .t
                    .get(target)
                    .cloned()
                    .ok_or(GraphError::MissingType(target))?;
                // Arguments are type contracts, not complete source-value evidence.
                // Written refinements constrain those contracts. Call inference
                // separates root value access modes with generic_type_argument.
                if !t.modes.relation.is_global() {
                    replacement.modes.relation = t.modes.relation;
                }
                if t.modes.uniqueness == Uniqueness::Unique {
                    replacement.modes.uniqueness = Uniqueness::Unique;
                }
                if t.modes.multiplicity == Multiplicity::Once {
                    replacement.modes.multiplicity = Multiplicity::Once;
                }
                if t.sendability_requirement == SendabilityRequirement::Sendable {
                    replacement.sendability_requirement = SendabilityRequirement::Sendable;
                }
                if !t.brand.alternatives.is_empty() {
                    replacement.brand = t.brand;
                }
                let mut target = self.t.intern(replacement.clone());
                // Qualifiers written on the parameter occurrence say nothing
                // about plain bits. Nominal arguments need their declaration:
                // callers holding a lookup normalize the substituted graph.
                if structural_property(
                    self.t,
                    target,
                    StructuralProperty::Plain,
                    &GenericDeclarations::default(),
                    &mut |_| Ok::<_, crate::QueryError>(None),
                )
                .unwrap()
                {
                    replacement.canonicalize_plain();
                    target = self.t.intern(replacement);
                }
                self.memo.insert(key, target);
                self.active.remove(&id);
                return Ok(target);
            }
        }
        if let Some(ErasedCaptures::Known(a)) = &mut t.erased_captures {
            self.authority(a, nested)?;
        }
        t.drop_allocation = self.drop_effect(t.drop_allocation, nested)?;
        t.modes.sendability = self.sendability(t.modes.sendability, nested)?;
        // Retain shared children until a substituted edge actually changes.
        // Borrowed from rustc15c1044 rustc_type_ir/src/fold.rs:296-325's
        // allocation-preserving fold. Hirsel's interned source remains shared,
        // so defer Arc::make_mut until an edge changes instead of taking it.
        t.kind = match &t.kind {
            TypeKind::Named { arguments, .. } | TypeKind::Builtin { arguments, .. } => {
                let mut kind = t.kind.clone();
                let (TypeKind::Named {
                    arguments: output, ..
                }
                | TypeKind::Builtin {
                    arguments: output, ..
                }) = &mut kind
                else {
                    unreachable!()
                };
                // Preserve argument order, including which malformed actual
                // reports its GraphError first, across type and size edges.
                for (i, argument) in arguments.iter().enumerate() {
                    let new = match argument {
                        GenericArgument::Type(ty) => GenericArgument::Type(self.ty(*ty, nested)?),
                        GenericArgument::Size(Size::Parameter(p)) => {
                            let (reference, selected) = self.reference(*p, nested);
                            if selected {
                                match self.argument(p.parameter().0)? {
                                    GenericArgument::Size(size) => {
                                        let mut size = size.clone();
                                        if let Size::Parameter(BinderRef::Bound { depth, .. }) =
                                            &mut size
                                        {
                                            *depth += nested;
                                        }
                                        GenericArgument::Size(size)
                                    }
                                    _ => return Err(GraphError::ArgumentKind(p.parameter().0)),
                                }
                            } else {
                                GenericArgument::Size(Size::Parameter(reference))
                            }
                        }
                        argument => argument.clone(),
                    };
                    if new != *argument {
                        output[i] = new;
                    }
                }
                kind
            }
            TypeKind::Error => return Err(GraphError::RecoveryType(id)),
            kind => {
                let depth = nested + u32::from(matches!(kind, TypeKind::Function(_)));
                kind.try_map_edges(|child| self.ty(child, depth))?
            }
        };
        if let TypeKind::Function(f) = &mut t.kind {
            if self.substitution.roles.result_type.is_some() {
                f.error = result_error(self.t, f.result, self.substitution.roles);
            }
            self.authority(&mut f.authority, nested + 1)?;
            if let Uses::Only(a) = &mut f.uses {
                self.authority(a, nested + 1)?;
            }
        }
        let result = self.t.intern(t);
        self.memo.insert(key, result);
        self.active.remove(&id);
        Ok(result)
    }
}
/// Direction in which a nested row of a received value may promise more than
/// the row expected there (CLO-2, #58). A value with a stronger promise serves
/// wherever it is only read: as a tuple, record, variant or box part, as a
/// sequence element and as a callable's result or error. A callable's
/// parameter is supplied by its caller, so the direction turns round there
/// ("written unrestricted parameters do not meet restricted uses"), and a
/// `mut` parameter, which the callee writes back, keeps the exact contract.
/// A nominal type's arguments stay exact: its declaration may use them in
/// either direction.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Variance {
    Exact,
    Covariant,
    Contravariant,
}
impl Variance {
    pub fn reversed(self) -> Self {
        match self {
            Self::Exact => Self::Exact,
            Self::Covariant => Self::Contravariant,
            Self::Contravariant => Self::Covariant,
        }
    }
    /// One entry per `TypeKind::graph_edges` edge of `kind`, in its order.
    pub fn edges(self, kind: &TypeKind) -> Vec<Self> {
        match kind {
            TypeKind::Function(f) => f
                .parameters
                .iter()
                .map(|p| match p.passing {
                    hirsel_syntax::PassingMode::Mut => Self::Exact,
                    _ => self.reversed(),
                })
                .chain([self])
                .chain(f.error.map(|_| self))
                .collect(),
            TypeKind::Builtin {
                kind: BuiltinType::Array | BuiltinType::List | BuiltinType::Inline,
                ..
            } => vec![self; kind.graph_edges().len()],
            TypeKind::Named { .. } | TypeKind::Builtin { .. } => {
                vec![Self::Exact; kind.graph_edges().len()]
            }
            _ => vec![self; kind.graph_edges().len()],
        }
    }
}
/// The directional walk behind `ContextMemo::compatible`: `actual`, whose root
/// row the root reception rules already adjusted to `actual_root`, is received
/// where `expected` stands. The adjusted row stands in for `actual`'s own row
/// at the root pair only; payload membership names `actual`. Exact sub-comparisons that no policy can
/// weaken are decided by semantic normal forms.
pub(crate) fn directional_equal(
    t: &TypeInterner,
    expected: TypeId,
    actual: TypeId,
    actual_root: Type,
    // A nested row of `actual` may carry a brand where `expected` names none.
    forget_brands: bool,
    payloads: Option<&HashSet<TypeId>>,
    // `actual` is a value received where `expected` is expected: its nested
    // rows may make the stronger promise along `Variance` edges.
    received: bool,
) -> Result<bool, GraphError> {
    // Both complete graphs must be valid even if their roots immediately
    // differ. The adjusted root keeps its row's kind and edges.
    t.validate(expected)?;
    t.validate(actual)?;
    let b_root = actual;
    let root = if received {
        Variance::Covariant
    } else {
        Variance::Exact
    };
    // `header`: the row is a callable's parameter or result.
    let mut pending = vec![(expected, b_root, payloads.is_some(), root, false)];
    let mut seen = IdSet::default();
    while let Some((left, right, view, variance, header)) = pending.pop() {
        if !seen.insert((left, right, view, variance, header)) {
            continue;
        }
        let adjusted_root = left == expected && right == b_root;
        if !adjusted_root && variance == Variance::Exact && !view && !forget_brands {
            // Nothing below can weaken: this is equality of receiving
            // contracts, current content summaries aside.
            if crate::normal::contract(t, left) != crate::normal::contract(t, right) {
                return Ok(false);
            }
            continue;
        }
        let children_view = view && payloads.is_some_and(|ids| ids.contains(&right));
        let left = t.row(left);
        let right_row = if adjusted_root {
            &actual_root
        } else {
            t.row(right)
        };
        let mut expected_row = crate::normal::semantic_row(left);
        let mut actual = crate::normal::semantic_row(right_row);
        strip_edges(&mut expected_row);
        strip_edges(&mut actual);
        if variance == Variance::Exact
            && (!matches!(
                actual.kind,
                TypeKind::Function(_)
                    | TypeKind::Builtin {
                        kind: BuiltinType::Iter,
                        ..
                    }
            ) || expected_row.sendability_requirement == SendabilityRequirement::Sendable)
            && expected_row.sendability_requirement == actual.sendability_requirement
        {
            // Mutable/owned structural equality compares fixed receiving
            // contracts. Current content is checked separately by receipt flow.
            actual.modes.sendability = expected_row.modes.sendability.clone();
        }
        if forget_brands && left.brand.alternatives.is_empty() {
            actual.brand = Brand::default();
        }
        // MODE-1: an owned payload read through a view may be bounded by less
        // than its receiving row allows, as a received root may: a global
        // payload (a deep copy, say) fills a frame-local slot's payload.
        if view
            && actual.modes.relation != left.modes.relation
            && crate::properties::receives_relation(
                &left.modes.relation,
                &actual.modes.relation,
                crate::properties::TypeRelation::Value,
            )
        {
            actual.modes.relation = left.modes.relation.clone();
        }
        if view
            && left.modes.uniqueness == Uniqueness::Aliased
            && actual.modes.uniqueness == Uniqueness::Unique
        {
            actual.modes.uniqueness = Uniqueness::Aliased;
            if left.modes.sendability == Sendability::NotSendable {
                actual.modes.sendability = Sendability::NotSendable;
            }
        }
        // MODE-2: a callable's result may promise uniqueness its required
        // type does not, and a parameter may ask for less than it is given.
        // Only the header row's root weakens, as a received value's root does.
        if header && header_weakens_uniqueness(&expected_row, &actual, variance) {
            actual.modes.uniqueness = expected_row.modes.uniqueness;
        }
        let subsumed = if adjusted_root {
            true
        } else {
            match variance {
                Variance::Exact => true,
                Variance::Covariant => {
                    crate::properties::nested_promise(&expected_row, &mut actual)
                }
                Variance::Contravariant => {
                    crate::properties::nested_promise(&actual, &mut expected_row)
                }
            }
        };
        if !subsumed || expected_row != actual {
            return Ok(false);
        }
        let children_header = matches!(left.kind, TypeKind::Function(_));
        pending.extend(
            left.kind
                .graph_edges()
                .into_iter()
                .zip(right_row.kind.graph_edges())
                .zip(variance.edges(&left.kind))
                .map(|((a, b), variance)| (a.ty, b.ty, children_view, variance, children_header)),
        );
    }
    Ok(true)
}

/// Row data compared independently of its edges, which the walk pairs.
fn strip_edges(row: &mut Type) {
    row.kind = row.kind.map_edges(|_| TypeId::DANGLING);
}

/// MODE-2 for a callable's header row read in `variance`: a covariant row
/// (a result) may be `unique` where `expected` is aliased, a contravariant row
/// (a borrowed or taken parameter) aliased where `expected` is `unique`.
/// A `mut` parameter is exact: the callee hands the value back.
pub fn header_weakens_uniqueness(expected: &Type, actual: &Type, variance: Variance) -> bool {
    match variance {
        Variance::Covariant => {
            expected.modes.uniqueness == Uniqueness::Aliased
                && actual.modes.uniqueness == Uniqueness::Unique
        }
        Variance::Contravariant => {
            expected.modes.uniqueness == Uniqueness::Unique
                && actual.modes.uniqueness == Uniqueness::Aliased
        }
        Variance::Exact => false,
    }
}

/// Visit origin-bearing rows without rebuilding them. Visits retain lexical
/// depth, declaration-order edges, and the rewriting walk's cycle/error policy.
pub fn visit_type_origins(
    store: &TypeInterner,
    root: TypeId,
    visit: impl FnMut(&Origin, u32) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    visit_type_origins_where(store, root, |_| true, visit)
}

/// `visit_type_origins` restricted to the relation places `place` selects;
/// brands are always visited. A checker that tracks only arena identity
/// passes its arena predicate.
pub fn visit_type_origins_where(
    store: &TypeInterner,
    root: TypeId,
    place: impl Fn(&Origin) -> bool,
    mut visit: impl FnMut(&Origin, u32) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    let mut pending = vec![(root, 0u32, false)];
    let mut active = IdSet::default();
    let mut done = IdSet::default();
    while let Some((id, depth, exit)) = pending.pop() {
        if exit {
            active.remove(&id);
            done.insert((id, depth));
            continue;
        }
        if done.contains(&(id, depth)) {
            continue;
        }
        if !active.insert(id) {
            return Err(GraphError::StructuralCycle(id));
        }
        let row = store.get(id).ok_or(GraphError::MissingType(id))?;
        hirsel_intern::work::count(hirsel_intern::work::Counter::ValidateOriginDependencyRows);
        for origin in &row.brand.alternatives {
            visit(origin, depth)?;
        }
        for reach in row.modes.relation.reaches() {
            if let Reach::Place(origin) = reach {
                if place(origin) {
                    visit(origin, depth)?;
                }
            }
        }
        pending.push((id, depth, true));
        pending.extend(
            row.kind
                .graph_edges()
                .into_iter()
                .rev()
                .map(|edge| (edge.ty, depth + edge.role.parameter_binders(), false)),
        );
    }
    Ok(())
}

/// Compare known type shape while allowing caller-designated inference holes.
/// Both complete graphs are still validated; recovery rows and cycles are errors.
/// This does not bind holes or change strict semantic/receiving comparisons.
pub fn shape_equal_with_holes(
    t: &TypeInterner,
    a_root: TypeId,
    b_root: TypeId,
    hole: impl Fn(&TypeKind) -> bool,
) -> Result<bool, GraphError> {
    fn arguments(
        a: &[GenericArgument],
        b: &[GenericArgument],
        pending: &mut Vec<(TypeId, TypeId)>,
    ) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(a, b)| match (a, b) {
                (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                    pending.push((*a, *b));
                    true
                }
                (GenericArgument::Size(a), GenericArgument::Size(b)) => a == b,
                _ => false,
            })
    }
    t.validate(a_root)?;
    t.validate(b_root)?;
    let mut pending = vec![(a_root, b_root)];
    let mut seen = IdSet::default();
    while let Some((a_id, b_id)) = pending.pop() {
        if !seen.insert((a_id, b_id)) {
            continue;
        }
        if t.shape(a_id) == t.shape(b_id) {
            continue;
        }
        let a = &t.row(a_id).kind;
        let b = &t.row(b_id).kind;
        if hole(a) || hole(b) {
            continue;
        }
        let equal = match (a, b) {
            (TypeKind::Primitive(a), TypeKind::Primitive(b)) => a == b,
            (TypeKind::Parameter(a), TypeKind::Parameter(b)) => a == b,
            (TypeKind::Tuple(a), TypeKind::Tuple(b)) => {
                pending.extend(a.iter().copied().zip(b.iter().copied()));
                a.len() == b.len()
            }
            (TypeKind::Box(a), TypeKind::Box(b)) => {
                pending.push((*a, *b));
                true
            }
            (
                TypeKind::Named {
                    definition: a,
                    arguments: aa,
                },
                TypeKind::Named {
                    definition: b,
                    arguments: ba,
                },
            ) => a == b && arguments(aa, ba, &mut pending),
            (
                TypeKind::Builtin {
                    kind: a,
                    arguments: aa,
                },
                TypeKind::Builtin {
                    kind: b,
                    arguments: ba,
                },
            ) => a == b && arguments(aa, ba, &mut pending),
            (TypeKind::Record(a), TypeKind::Record(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| {
                        pending.push((a.ty, b.ty));
                        a.name == b.name && a.growable == b.growable
                    })
            }
            (TypeKind::Variant(a), TypeKind::Variant(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| {
                        a.name == b.name
                            && a.fields.len() == b.fields.len()
                            && a.fields.iter().zip(&b.fields).all(|(a, b)| {
                                pending.push((a.1, b.1));
                                a.0 == b.0
                            })
                    })
            }
            (TypeKind::Function(a), TypeKind::Function(b)) => {
                if a.generics != b.generics || a.parameters.len() != b.parameters.len() {
                    return Ok(false);
                }
                pending.push((a.result, b.result));
                match (a.error, b.error) {
                    (Some(a), Some(b)) => pending.push((a, b)),
                    (None, None) => (),
                    _ => return Ok(false),
                }
                a.parameters.iter().zip(&b.parameters).all(|(a, b)| {
                    pending.push((a.ty, b.ty));
                    a.label == b.label && a.passing == b.passing
                })
            }
            _ => false,
        };
        if !equal {
            return Ok(false);
        }
    }
    Ok(true)
}
