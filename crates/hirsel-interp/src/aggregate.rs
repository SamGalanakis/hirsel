//! Nominal runtime facts are supplied by the driver from declared signatures.
use crate::{Error, Value, invalid};
use hirsel_syntax::{ContentHash, DefPath};
use hirsel_tir::{BodyId, Constructor};
use hirsel_types::{BuiltinType, QueryError, TypeInterner, TypeKind, TypeMemo};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nominal {
    pub definition: DefPath,
    pub resource: bool,
    pub distinct: bool,
    /// A resource's owner definition contains its drop body (revision 1).
    pub finalizer: Option<(DefPath, BodyId)>,
    pub fields: usize,
    /// The private representation's shape, which a bounded arena's logical
    /// charge reads (MEM-6); None for an opaque nominal.
    pub representation: Option<hirsel_types::TypeId>,
}
/// The declared views codecs and reflection read: a recorded view. Each read
/// of a path records the digest of the view it saw (or its absence) into the
/// innermost open recording, so the evaluation that read it can be retired
/// when the view changes, an opaque owner's private shape included.
/// A declared view held by identity: each read is a scoped view of the
/// signature from its cache, which may have evicted and recomputed it, or
/// the cache's refusal to produce it again.
pub trait ViewIdentity: Send + Sync + std::fmt::Debug {
    fn definition(&self) -> hirsel_syntax::DefId;
    fn signature(&self) -> Result<hirsel_types::SignatureRef, hirsel_types::QueryError>;
}
/// One declared view a serialization names: held, or by identity.
#[derive(Clone, Debug)]
pub enum SerializedView {
    Held(hirsel_types::SignatureRef),
    Identified(Arc<dyn ViewIdentity>),
}
impl SerializedView {
    /// A scoped view of the signature, pinning its cache row only while held.
    pub fn signature(&self) -> Result<hirsel_types::SignatureRef, hirsel_types::QueryError> {
        match self {
            Self::Held(signature) => Ok(signature.clone()),
            Self::Identified(identity) => identity.signature(),
        }
    }
    fn definition(&self) -> hirsel_syntax::DefId {
        match self {
            Self::Held(signature) => signature.definition,
            Self::Identified(identity) => identity.definition(),
        }
    }
}
impl From<hirsel_types::SignatureRef> for SerializedView {
    fn from(signature: hirsel_types::SignatureRef) -> Self {
        Self::Held(signature)
    }
}
impl From<Arc<dyn ViewIdentity>> for SerializedView {
    fn from(identity: Arc<dyn ViewIdentity>) -> Self {
        Self::Identified(identity)
    }
}
/// Where a demanded catalog reads its rows: one declared view per path,
/// produced only when an evaluation names that path.
pub trait TypeRows: Send + Sync {
    /// `path`'s declared view with its digest; None when `path` declares
    /// nothing. Err is the row's own failure (a refusal included).
    fn view(&self, path: &DefPath) -> Result<Option<(SerializedView, ContentHash)>, QueryError>;
    /// The body that finalizes a resource whose drop is `drop`.
    fn finalizer(&self, drop: &DefPath) -> Result<Option<(DefPath, BodyId)>, QueryError>;
}
/// One settled row of a demanded catalog.
struct Row {
    view: SerializedView,
    digest: ContentHash,
    nominal: Option<Nominal>,
}
type Demand = Result<Option<Arc<Row>>, QueryError>;
/// The views a serialization reads: a held table, or rows demanded per path
/// and kept once settled.
enum Views {
    Held(BTreeMap<DefPath, (SerializedView, ContentHash)>),
    Demanded {
        rows: Arc<dyn TypeRows>,
        types: Arc<TypeInterner>,
        settled: std::sync::Mutex<std::collections::HashMap<DefPath, Demand>>,
    },
}
impl Default for Views {
    fn default() -> Self {
        Self::Held(BTreeMap::new())
    }
}
impl Views {
    fn row(&self, path: &DefPath) -> Result<Option<Arc<Row>>, QueryError> {
        let Self::Demanded {
            rows,
            types,
            settled,
        } = self
        else {
            return Ok(None);
        };
        if let Some(demand) = settled.lock().unwrap().get(path) {
            return demand.clone();
        }
        let demand = Self::demand(&**rows, types, path);
        match &demand {
            // A refusal settles nothing: a later read demands again.
            Err(_) => demand,
            Ok(_) => settled
                .lock()
                .unwrap()
                .entry(path.clone())
                .or_insert(demand)
                .clone(),
        }
    }
    fn demand(rows: &dyn TypeRows, types: &TypeInterner, path: &DefPath) -> Demand {
        let Some((view, digest)) = rows.view(path)? else {
            return Ok(None);
        };
        let nominal = Nominal::of(types, &*view.signature()?, |drop| rows.finalizer(drop))?;
        Ok(Some(Arc::new(Row {
            view,
            digest,
            nominal,
        })))
    }
    /// `path`'s view and digest.
    fn get(&self, path: &DefPath) -> Result<Option<(SerializedView, ContentHash)>, QueryError> {
        match self {
            Self::Held(views) => Ok(views.get(path).cloned()),
            Self::Demanded { .. } => Ok(self.row(path)?.map(|row| (row.view.clone(), row.digest))),
        }
    }
}
impl Nominal {
    /// The runtime facts of `signature` when it declares a type.
    pub fn of(
        types: &TypeInterner,
        signature: &hirsel_types::Signature,
        mut finalizer: impl FnMut(&DefPath) -> Result<Option<(DefPath, BodyId)>, QueryError>,
    ) -> Result<Option<Self>, QueryError> {
        let hirsel_types::SignatureKind::Type(t) = &signature.kind else {
            return Ok(None);
        };
        let fields = t
            .representation
            .and_then(|id| types.get(id))
            .map(|r| match &r.kind {
                TypeKind::Record(fs) => fs.len(),
                TypeKind::Variant(vs) => vs.iter().map(|v| v.fields.len()).max().unwrap_or(0),
                _ => 1,
            })
            .unwrap_or(0);
        let finalizer = match &t.drop {
            Some(drop) => finalizer(drop)?,
            None => None,
        };
        Ok(Some(Self {
            definition: signature.definition.path().clone(),
            resource: t.resource,
            distinct: t.distinct,
            finalizer,
            fields,
            representation: t.representation,
        }))
    }
}
#[derive(Default)]
pub struct Serialization {
    views: Arc<Views>,
    recordings: std::sync::Mutex<Vec<BTreeMap<DefPath, ContentHash>>>,
}
impl Clone for Serialization {
    /// The same views, with no recording open.
    fn clone(&self) -> Self {
        Self {
            views: self.views.clone(),
            recordings: Default::default(),
        }
    }
}
impl std::fmt::Debug for Serialization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &*self.views {
            Views::Held(views) => f.debug_set().entries(views.keys()).finish(),
            Views::Demanded { settled, .. } => f
                .debug_set()
                .entries(settled.lock().unwrap().keys())
                .finish(),
        }
    }
}
impl Serialization {
    /// Each path's view with its digest.
    pub fn new(views: impl IntoIterator<Item = (SerializedView, ContentHash)>) -> Self {
        Self {
            views: Arc::new(Views::Held(
                views
                    .into_iter()
                    .map(|(view, digest)| (view.definition().path().clone(), (view, digest)))
                    .collect(),
            )),
            recordings: Default::default(),
        }
    }
    /// Views read from `rows`, one row per path an evaluation names. Each
    /// row also answers its nominal's runtime facts.
    pub fn demanded(types: Arc<TypeInterner>, rows: Arc<dyn TypeRows>) -> Self {
        Self {
            views: Arc::new(Views::Demanded {
                rows,
                types,
                settled: Default::default(),
            }),
            recordings: Default::default(),
        }
    }
    /// A scoped view of `path`'s signature, recording the read. A row that
    /// A scoped view of `path`'s signature, recording the read. A row that
    /// fails, or a view its cache refuses to produce again, refuses the read.
    pub fn get(&self, path: &DefPath) -> Result<Option<hirsel_types::SignatureRef>, QueryError> {
        let view = self.views.get(path)?;
        if let Some(recording) = self.recordings.lock().unwrap().last_mut() {
            recording.insert(
                path.clone(),
                view.as_ref().map_or_else(ContentHash::default, |v| v.1),
            );
        }
        view.map(|(view, _)| view.signature()).transpose()
    }
    /// Whether `path`'s view is held or its demanded row has settled.
    fn holds(&self, path: &DefPath) -> bool {
        match &*self.views {
            Views::Held(views) => views.contains_key(path),
            Views::Demanded { settled, .. } => {
                matches!(settled.lock().unwrap().get(path), Some(Ok(Some(_))))
            }
        }
    }
    /// `path`'s nominal facts from a demanded row; a held table has none.
    fn nominal(&self, path: &DefPath) -> Result<Option<Nominal>, QueryError> {
        Ok(self.views.row(path)?.and_then(|row| row.nominal.clone()))
    }
    /// Open a recording: reads go to it until the matching `finish`.
    pub fn record(&self) {
        self.recordings.lock().unwrap().push(BTreeMap::new());
    }
    /// Close the innermost recording and return its reads, which also join
    /// the enclosing recording: an evaluation's reads include its nested ones.
    pub fn finish(&self) -> BTreeMap<DefPath, ContentHash> {
        let mut recordings = self.recordings.lock().unwrap();
        let reads = recordings.pop().unwrap_or_default();
        if let Some(outer) = recordings.last_mut() {
            outer.extend(reads.iter().map(|(path, digest)| (path.clone(), *digest)));
        }
        reads
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeTypes {
    /// The session interner: every TypeId the run reads is one of its rows.
    pub types: Arc<TypeInterner>,
    /// Structural substitution memo for closing invocation types.
    pub memo: Arc<TypeMemo>,
    /// Declared snapshots used to prove and specialize codec schemas.
    pub serialization: Serialization,
    /// Full for diagnostic clients; Effects for ordinary observed execution.
    pub trace_mode: crate::TraceMode,
    pub nominals: Vec<Nominal>,
    pub forget: Option<DefPath>,
    /// Present for checked execution; absent runs normally.
    pub checked: Option<crate::checked::CheckedRuntime>,
    /// The host's run budgets; the default bounds nothing.
    pub budgets: crate::RunBudgets,
}
impl RuntimeTypes {
    /// Runtime facts over `types` with no nominals, checking or budgets.
    pub fn new(types: Arc<TypeInterner>) -> Self {
        Self {
            types,
            memo: Arc::new(TypeMemo::new()),
            serialization: Default::default(),
            trace_mode: Default::default(),
            nominals: Vec::new(),
            forget: None,
            checked: None,
            budgets: Default::default(),
        }
    }
    /// Whether `path` has a row: an explicit nominal, a held view or a
    /// settled demanded row. Asking demands nothing.
    pub fn has_row(&self, path: &DefPath) -> bool {
        self.nominals.iter().any(|n| n.definition == *path) || self.serialization.holds(path)
    }
    /// `path`'s nominal facts: an explicit row, else its demanded row.
    pub fn get(&self, path: &DefPath) -> Result<Option<Nominal>, QueryError> {
        match self.nominals.iter().find(|n| n.definition == *path) {
            Some(nominal) => Ok(Some(nominal.clone())),
            None => self.serialization.nominal(path),
        }
    }
}
pub(crate) fn construct(
    constructor: &Constructor,
    values: Vec<Option<Value>>,
    kind: &TypeKind,
    types: &RuntimeTypes,
    sizes: Vec<Option<u64>>,
) -> Result<Value, Error> {
    Ok(match constructor {
        Constructor::Tuple if values.is_empty() => Value::Unit,
        Constructor::Tuple => Value::Tuple(values),
        Constructor::Variant {
            definition,
            variant,
        } => {
            let nominal = types.get(definition).map_err(Error::Query)?;
            if matches!(kind, TypeKind::Named { .. }) && nominal.is_none() {
                return Err(Error::Unavailable(
                    "nominal runtime metadata required".into(),
                ));
            }
            if nominal.is_some_and(|n| n.resource) {
                Value::ResourceVariant {
                    sizes: sizes.into(),
                    definition: definition.into(),
                    variant: *variant,
                    fields: values,
                }
            } else {
                Value::Variant {
                    variant: *variant,
                    fields: values,
                }
            }
        }
        Constructor::Record(definition) => {
            if let Some(n) = types.get(definition).map_err(Error::Query)? {
                if n.fields != values.len() {
                    return Err(invalid("nominal constructor arity"));
                }
                if n.resource {
                    Value::Resource {
                        sizes: sizes.into(),
                        definition: definition.into(),
                        fields: values,
                    }
                } else if n.distinct {
                    Value::Distinct {
                        definition: definition.into(),
                        fields: values,
                    }
                } else {
                    Value::Record {
                        definition: definition.into(),
                        fields: values,
                    }
                }
            } else {
                if matches!(kind, TypeKind::Named { .. }) {
                    return Err(Error::Unavailable(
                        "nominal runtime metadata required".into(),
                    ));
                }
                Value::Record {
                    definition: definition.into(),
                    fields: values,
                }
            }
        }
        Constructor::Sequence => {
            let TypeKind::Builtin { kind, .. } = kind else {
                return Err(invalid("sequence requires builtin type"));
            };
            if !matches!(
                kind,
                BuiltinType::Array | BuiltinType::List | BuiltinType::Inline | BuiltinType::Simd
            ) {
                return Err(invalid("unsupported sequence type"));
            }
            Value::Sequence {
                kind: *kind,
                elements: values.into(),
                arena: None,
            }
        }
        Constructor::SortedMap { .. } => {
            if !matches!(
                kind,
                TypeKind::Builtin {
                    kind: BuiltinType::SortedMap,
                    ..
                }
            ) || values.len() != 1
                || values[0].is_none()
            {
                return Err(invalid(
                    "sorted map constructor requires Order and SortedMap type",
                ));
            }
            Value::Record {
                definition: (&DefPath::core("SortedMap")).into(),
                fields: vec![
                    values.into_iter().next().unwrap(),
                    Some(Value::Sequence {
                        kind: BuiltinType::Array,
                        elements: Default::default(),
                        arena: None,
                    }),
                ],
            }
        }
        Constructor::Map => {
            if !matches!(
                kind,
                TypeKind::Builtin {
                    kind: BuiltinType::Map,
                    ..
                }
            ) {
                return Err(invalid("map constructor requires Map type"));
            }
            if values.iter().any(|value| !matches!(value, Some(Value::Tuple(fields)) if fields.len() == 2 && fields.iter().all(Option::is_some))) {
                return Err(invalid("map constructor requires complete key/value entries"));
            }
            Value::map(values)
        }
        Constructor::Set => {
            if !matches!(
                kind,
                TypeKind::Builtin {
                    kind: BuiltinType::Set,
                    ..
                }
            ) || values.iter().any(Option::is_none)
            {
                return Err(invalid("set constructor requires complete Set elements"));
            }
            Value::set(values)
        }
        Constructor::Box => {
            if values.len() != 1 {
                return Err(invalid("box arity"));
            }
            Value::Box(values)
        }
    })
}
/// Suppress finalizers and owned-field destruction, while freeing simulation storage.
pub(crate) fn forget(value: Value) -> Result<(), Error> {
    if !matches!(
        value,
        Value::Resource { .. } | Value::ResourceVariant { .. }
    ) {
        return Err(invalid("forget requires resource"));
    }
    let mut work = vec![value];
    while let Some(mut v) = work.pop() {
        if let Some(fields) = v.fields_mut() {
            work.extend(fields.iter_mut().filter_map(Option::take));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_collection_constructors_preserve_order_and_reject_partial_entries() {
        let runtime = RuntimeTypes::new(TypeInterner::new(None));
        let map = TypeKind::Builtin {
            kind: BuiltinType::Map,
            arguments: vec![].into(),
        };
        let entries = vec![
            Some(Value::Tuple(vec![
                Some(Value::Bool(true)),
                Some(Value::Unit),
            ])),
            Some(Value::Tuple(vec![
                Some(Value::Bool(false)),
                Some(Value::Unit),
            ])),
        ];
        assert_eq!(
            construct(&Constructor::Map, entries.clone(), &map, &runtime, vec![]).unwrap(),
            Value::map(entries)
        );
        assert!(
            construct(
                &Constructor::Map,
                vec![Some(Value::Tuple(vec![Some(Value::Bool(true)), None]))],
                &map,
                &runtime,
                vec![]
            )
            .is_err()
        );
        let set = TypeKind::Builtin {
            kind: BuiltinType::Set,
            arguments: vec![].into(),
        };
        let elements = vec![Some(Value::Bool(true)), Some(Value::Bool(false))];
        assert_eq!(
            construct(&Constructor::Set, elements.clone(), &set, &runtime, vec![]).unwrap(),
            Value::set(elements)
        );
        assert!(construct(&Constructor::Set, vec![None], &set, &runtime, vec![]).is_err());
        assert!(construct(&Constructor::Map, vec![], &set, &runtime, vec![]).is_err());
        assert!(construct(&Constructor::Set, vec![], &map, &runtime, vec![]).is_err());
    }
}
