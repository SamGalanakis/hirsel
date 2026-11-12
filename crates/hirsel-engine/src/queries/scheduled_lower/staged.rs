//! Size selections use the ordinary Evaluate factory and descriptor store.
//! The declaration and actuals determine a distinct immutable query identity.
use super::*;
#[derive(Clone)]
pub(super) struct Selection {
    pub declaration: DefId,
    pub owner: DefId,
    pub arguments: Vec<u64>,
    pub instance: DefId,
    pub key: EntryKey,
    waiting: Option<Span>,
}
pub(super) struct Staged {
    pub declarations: Arc<inputs::Shared<BTreeMap<DefId, crate::comptime::StagedConstant>>>,
    selections: Mutex<BTreeMap<ContentHash, Selection>>,
}
impl Staged {
    pub(super) fn with_request_site<R>(
        &self,
        id: QueryId,
        update: impl FnOnce(&mut Option<Span>) -> R,
    ) -> Option<R> {
        if id.kind != FRONT_EVALUATE_KIND {
            return None;
        }
        let mut selections = self.selections.lock().unwrap_or_else(|e| e.into_inner());
        Some(update(&mut selections.get_mut(&id.address.0)?.waiting))
    }
    pub(super) fn new(
        declarations: &BTreeMap<DefPath, crate::comptime::StagedConstant>,
        previous: Option<&Self>,
        partition: &LowerPartition,
    ) -> Option<Self> {
        let previous = previous.filter(|previous| {
            declarations.len() == previous.declarations.len()
                && declarations.iter().all(|(path, current)| {
                    previous
                        .declarations
                        .get(&DefId::intern(path))
                        .is_some_and(|previous| {
                            previous.owner == current.owner
                                && previous.expression == current.expression
                                && previous.inputs == current.inputs
                                && previous.signature.fingerprint == current.signature.fingerprint
                        })
                })
        });
        let declarations = match previous {
            Some(previous) => previous.declarations.clone(),
            None => {
                let mut weight = hirsel_query_products::retained::MetadataWeight::default();
                if !declarations.is_empty() {
                    weight.tree::<DefId, crate::comptime::StagedConstant>(declarations.len());
                }
                for row in declarations.values() {
                    weight.path(&row.owner);
                    weight.flat_vec(&row.inputs);
                }
                inputs::Shared::new(
                    declarations
                        .iter()
                        .map(|(path, row)| (DefId::intern(path), row.clone()))
                        .collect(),
                    weight.bytes(),
                    partition,
                )?
            }
        };
        Some(Self {
            declarations,
            selections: Mutex::new(BTreeMap::new()),
        })
    }
    pub(super) fn selection(&self, id: QueryId) -> Option<Selection> {
        if id.kind != FRONT_EVALUATE_KIND {
            return None;
        }
        self.selections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&id.address.0)
            .cloned()
    }
    pub(super) fn selected(&self, path: &DefPath) -> Option<QueryId> {
        self.selections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|(_, row)| row.instance.path() == path)
            .map(|(hash, _)| QueryId {
                kind: FRONT_EVALUATE_KIND,
                address: hirsel_artifact_store::Address(*hash),
            })
    }
    pub(super) fn instances(&self) -> Vec<DefPath> {
        self.selections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .map(|row| row.instance.path().clone())
            .collect()
    }
}
impl FrontLowerQueries {
    /// Select a declared staged constant under its exact closed size actuals.
    /// This only interns its admitted Evaluate address. The caller requests
    /// the query on the ordinary runtime before reading a settled value.
    pub fn staged_query(&self, path: &DefPath, arguments: &[u64]) -> Result<QueryId, QueryError> {
        let id = DefId::intern(path);
        let declaration = self
            .staged
            .declarations
            .get(&id)
            .ok_or(QueryError::NotImplemented)?;
        if arguments.len() != declaration.inputs.len() {
            return Err(QueryError::NotImplemented);
        }
        let owner = DefId::intern(&declaration.owner);
        let original = &self.inputs[owner.path()].evaluate;
        let bytes =
            bincode::serialize(&(path, arguments)).map_err(|_| QueryError::NotImplemented)?;
        let hash = ContentHash::of_parts(
            "hirsel.front.staged-evaluate.v1",
            [original.0.0.as_slice(), bytes.as_slice()],
        );
        let query = self.namespace.query_id(FRONT_EVALUATE_KIND, hash);
        let live = query.address.0;
        let mut selections = self
            .staged
            .selections
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !selections.contains_key(&live) {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            let mut previous = hirsel_query_products::retained::MetadataWeight::default();
            weight.tree::<ContentHash, Selection>(selections.len() + 1);
            if !selections.is_empty() {
                previous.tree::<ContentHash, Selection>(selections.len());
            }
            let required = weight
                .bytes()
                .checked_sub(previous.bytes())
                .and_then(|bytes| bytes.checked_add(size_of_val(arguments) as u64))
                .ok_or(QueryError::NotImplemented)?;
            self.retained
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .grow(required)
                .ok_or(QueryError::NotImplemented)?;
            selections.insert(
                live,
                Selection {
                    declaration: id,
                    owner,
                    arguments: arguments.to_vec(),
                    instance: DefId::intern(&hirsel_tir::constant_instance_path(path, arguments)),
                    key: EntryKey(hash, original.1, QueryKind::Evaluate),
                    waiting: None,
                },
            );
        }
        Ok(query)
    }
}

// Preserve request order and repetitions. A semantic read set cannot recover
// which concrete size instances the interpreter actually selected.
pub(super) type Selections = Vec<(DefId, Vec<u64>)>;
pub(super) fn selection_bytes(rows: &Selections) -> u64 {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.vec(rows, |weight, (_, arguments)| weight.flat_vec(arguments));
    weight.bytes()
}
impl FrontLowerQueries {
    pub(super) fn restore_selections<'r>(
        &self,
        selections: &Selections,
        reads: &'r QueryReads<'r>,
    ) -> Result<(), Reference> {
        for (declaration, arguments) in selections {
            let id = self
                .staged_query(declaration.path(), arguments)
                .map_err(|_| self.errors.resource)?;
            let selected = reads.read_consuming(id);
            let outcome = self.selected(id, selected.clone()).ok_or_else(|| {
                selected
                    .as_ref()
                    .err()
                    .copied()
                    .unwrap_or(self.errors.protocol)
            })?;
            if !matches!(outcome.result, Ok(Value::Constant(_))) {
                return Err(selected.err().unwrap_or(self.errors.protocol));
            }
        }
        Ok(())
    }
}

impl FrontLowerQueries {
    pub(crate) fn staged_failure(
        &self,
        id: QueryId,
        reference: Reference,
        site: Option<Span>,
    ) -> Option<QueryError> {
        let mut error = self.failure(reference)?;
        if let Some(site) = site
            && let QueryError::Diagnostics(diagnostics) = &mut error
        {
            let selection = self.staged.selection(id)?;
            for diagnostic in diagnostics {
                diagnostic.secondary.push(hirsel_diag::Label {
                    span: site,
                    message: format!("selected size instantiation {:?}", selection.arguments),
                });
            }
        }
        Some(error)
    }
}

impl FrontLowerQueries {
    /// A selected staged value has a closed link identity, rather than an
    /// ordinary source declaration. Keep its typed header and the generic
    /// declaration it instantiates together when publishing execution data.
    pub(crate) fn settled_constant_header(
        &self,
        path: &DefPath,
    ) -> Result<Option<(hirsel_types::SignatureRef, hirsel_types::SignatureRef)>, QueryError> {
        let selection = self
            .staged
            .selections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|selection| selection.instance.path() == path)
            .cloned();
        let Some(selection) = selection else {
            return Ok(None);
        };
        let declaration = &self.staged.declarations[&selection.declaration];
        let value = self.settled_constant(path, &[])?;
        let signature = closed_constant_header(
            self.namespace.snapshot.types(),
            &self.memo,
            &declaration.signature,
            &selection.arguments,
            &value,
        )?;
        Ok(Some((Arc::new(signature), declaration.signature.clone())))
    }

    /// Lower and reopened descriptors record the exact staged selections they
    /// consumed. Native statics must demand their Evaluate products in this
    /// runtime before collecting values; a prior graph's completion is not one.
    pub(crate) fn staged_static_query(
        &self,
        selected: &[DefPath],
        completed: &[QueryId],
    ) -> Option<QueryId> {
        self.staged
            .selections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, selection)| selected.contains(selection.owner.path()))
            .map(|(address, _)| QueryId {
                kind: FRONT_EVALUATE_KIND,
                address: hirsel_artifact_store::Address(*address),
            })
            .find(|query| !completed.contains(query))
    }
    pub(crate) fn staged_static_inputs(
        &self,
        selected: &BTreeSet<DefPath>,
        allowance: u64,
    ) -> Result<(crate::NativeStaticInputs, u64), QueryError> {
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "native staged constants".into(),
                message: "selected staged values exceed their native output admission".into(),
            })
        };
        // Keep the same lock order as settled_constants. Only completed
        // Evaluate products of a selected source owner enter native data.
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let selections = self
            .staged
            .selections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let values = || {
            selections.iter().filter_map(|(address, selection)| {
                if !selected.contains(selection.owner.path()) {
                    return None;
                }
                let (_, outcome, _) = outcomes.get(&(FRONT_EVALUATE_KIND, *address))?;
                let Ok(Value::Constant(value)) = &outcome.result else {
                    return None;
                };
                Some((selection.instance.path(), value))
            })
        };
        let declarations = || {
            self.staged
                .declarations
                .iter()
                .filter(|(_, declaration)| selected.contains(&declaration.owner))
        };
        if values().next().is_none() && declarations().next().is_none() {
            return Ok((
                crate::NativeStaticInputs {
                    constants: BTreeMap::new(),
                    definitions: vec![],
                    generated_signatures: BTreeMap::new(),
                },
                0,
            ));
        }
        let mut metadata = hirsel_query_products::retained::MetadataWeight::default();
        metadata.tree::<DefPath, Materialized>(values().count());
        metadata.tree::<DefPath, hirsel_types::SignatureRef>(declarations().count());
        let mut working = 0;
        for (path, value) in values() {
            metadata.path(path);
            metadata.add(value.root_clone_bytes().try_into().map_err(|_| refused())?);
            working = working.max(value.working_bytes());
        }
        for (path, _) in declarations() {
            metadata.path(path.path());
        }
        // Native requirement discovery consumes a second value copy before
        // publishing its graph. Admit that overlap beside the retained statics.
        let retained = metadata.bytes().checked_mul(2).ok_or_else(refused)?;
        if retained
            .checked_add(working)
            .is_none_or(|bytes| bytes > allowance)
        {
            return Err(refused());
        }
        let mut constants = BTreeMap::new();
        for (path, value) in values() {
            let (value, _) = value
                .decoded(
                    &self.records,
                    &*self.namespace.anchors,
                    &*self.store,
                    allowance,
                )
                .ok_or_else(refused)?;
            constants.insert(path.clone(), value);
        }
        let generated_signatures = declarations()
            .map(|(path, declaration)| (path.path().clone(), declaration.signature.clone()))
            .collect();
        Ok((
            crate::NativeStaticInputs {
                constants,
                definitions: vec![],
                generated_signatures,
            },
            retained,
        ))
    }
}

fn closed_constant_header(
    types: &hirsel_types::TypeInterner,
    memo: &hirsel_types::TypeMemo,
    declaration: &hirsel_types::Signature,
    arguments: &[u64],
    value: &Materialized,
) -> Result<hirsel_types::Signature, QueryError> {
    use hirsel_types::{GenericArgument, GenericBinder, GenericParameter, SignatureKind, Size};
    let refused = || {
        QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "closed constant publication".into(),
            message: "selected value disagrees with its generic declaration".into(),
        })
    };
    let SignatureKind::Const { ty, generics } = &declaration.kind else {
        return Err(refused());
    };
    if generics.len() != arguments.len()
        || !generics
            .iter()
            .all(|p| matches!(p, GenericParameter::Size { .. }))
    {
        return Err(refused());
    }
    let actuals = arguments
        .iter()
        .map(|n| GenericArgument::Size(Size::Constant(*n)))
        .collect::<Vec<_>>();
    let ty = hirsel_types::substitute(
        types,
        memo,
        *ty,
        &hirsel_types::Substitution {
            binder: Some(GenericBinder::Bound(0)),
            arguments: &actuals,
            ..Default::default()
        },
    )
    .map_err(|_| refused())?;
    if ty != value.root.ty {
        return Err(refused());
    }
    let mut signature = declaration.clone();
    signature.definition = DefId::intern(&hirsel_tir::constant_instance_path(
        declaration.definition.path(),
        arguments,
    ));
    signature.kind = SignatureKind::Const {
        ty,
        generics: vec![].into(),
    };
    signature.fingerprint =
        hirsel_types::encoding::fingerprint(types, &signature, &[]).map_err(|_| refused())?;
    Ok(signature)
}
