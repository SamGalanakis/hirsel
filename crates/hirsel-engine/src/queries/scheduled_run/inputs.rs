//! Semantic input construction is demanded per definition under one operation lease.
use super::*;
impl FrontRunQueries {
    pub(super) fn input(&self, path: &DefPath) -> Result<Arc<Input>, QueryError> {
        if !self.inputs.contains(path) {
            return Err(operational(format!("missing signature input for {path}")));
        }
        let mut input = self.prepare_input(path)?;
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Input>() + 2 * std::mem::size_of::<usize>());
        weight.tree::<DefPath, ()>(input.dependencies.len());
        for dependency in &input.dependencies {
            weight.path(dependency);
        }
        let bytes = weight.bytes();
        let mut remaining = self
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| operational("demanded Run input exceeds admission"))?;
        drop(remaining);
        input._charge = Some(InputCharge {
            remaining: self.retention.clone(),
            _allowance: self.ownership._allowance.clone(),
            bytes,
        });
        Ok(Arc::new(input))
    }
    pub(super) fn admit_decode(&self, bytes: u64) -> Result<Arc<InputCharge>, QueryError> {
        self.execution_admission().reserve(bytes)
    }
    pub(super) fn execution_admission(&self) -> ExecutionAdmission {
        ExecutionAdmission {
            remaining: self.retention.clone(),
            allowance: self.ownership._allowance.clone(),
            scratch: self.weight.scratch,
        }
    }
    fn prepare_input(&self, path: &DefPath) -> Result<Input, QueryError> {
        let namespace = self.operation.capture();
        if let Some(record) = namespace.installed_input(path, self.weight.scratch)? {
            if record.ownership.header.len > self.weight.scratch {
                return Err(operational("installed Run input exceeds scratch admission"));
            }
            let bytes = self
                .store
                .get_content(record.ownership.header)
                .ok_or_else(|| operational("missing installed header"))?;
            if record.ownership.header.domain.reference(&bytes) != Some(record.ownership.header) {
                return Err(operational("corrupt installed header"));
            }
            let requirement = self
                .records
                .installed_header(&self.anchors, bytes)
                .ok_or_else(|| operational("invalid installed header"))?;
            let charge = self.admit_decode(requirement.required_bytes())?;
            let (signature, owner_signature) = requirement
                .decode(&self.ownership._allowance)
                .ok_or_else(|| operational("invalid installed header"))?;
            signature.metadata.retention.retain(charge.clone());
            owner_signature.metadata.retention.retain(charge);
            return Ok(Input {
                _charge: None,
                signature,
                owner_signature,
                content: record.ownership.semantic,
                specializations: None,
                dependencies: record.semantic_dependencies.into_iter().collect(),
                source: true,
                executable: record.discovery.executable,
                generated: record.discovery.generator && !record.discovery.instance_bodied,
                implementation: record.implementation,
            });
        }
        let signatures = &namespace.snapshot;
        let declarations = &namespace.declarations;
        let source = namespace.source_paths.iter().any(|id| id.path() == path);
        let namespace::HeaderInput {
            output,
            reads: signature_reads,
            semantic,
        } = namespace.header(path)?;
        let signature = output.value.clone();
        let owner_signature = signatures
            .body_view(&path.0.package, DefId::intern(path))
            .ok_or(QueryError::NotImplemented)??
            .clone();
        let mut content = vec![
            hirsel_types::encoding::canonical_encoding(
                namespace.snapshot.types(),
                &signature,
                &output.dependencies,
            )
            .map_err(|e| operational(format!("signature encoding: {e:?}")))?,
        ];
        let mut dependencies: BTreeSet<_> = output
            .dependencies
            .iter()
            .map(|d| d.definition.path().clone())
            .collect();
        // Scalar semantic_body_node follows source body observations only for
        // callable/value items. Type declarations contribute their normalized
        // signature and explicit drop/fact/check edges, not header lookups.
        if !matches!(signature.kind, SignatureKind::Type(_)) {
            if let Some(reads) = signature_reads {
                extend_reads(&mut dependencies, &reads, |key| {
                    FrontBodyQueries::lookup_in(signatures, declarations, key)
                });
            }
            dependencies.extend(semantic.into_iter().map(|id| id.path().clone()));
        }
        let mut executable = false;
        let mut generated = false;
        let mut implementation = ContentHash::default();
        if source {
            let syntax = namespace.syntax(path)?;
            executable = generation::executable(&syntax);
            generated = generation::generator(&syntax).is_some();
            implementation = syntax.text_hash;
            if executable || !matches!(signature.kind, SignatureKind::Type(_)) {
                content.extend(
                    hirsel_fmt::semantic_item(&syntax)
                        .ok_or_else(|| operational("invalid semantic source item"))?,
                );
            }
        }
        if let SignatureKind::Type(declaration) = &signature.kind {
            dependencies.extend(declaration.drop.iter().cloned());
            if declaration.fact.is_some() {
                dependencies.insert(hirsel_check_protocol::fact::fact_path(path));
            }
            if let Some(check) = &declaration.serializable_check {
                dependencies.insert(check.function.clone());
            }
        }
        let specializations = self.specializations.get(path).cloned();
        let bytes =
            bincode::serialize(&content).map_err(|_| operational("Run input content encoding"))?;
        let content = self
            .store
            .put_content(INPUT_CONTENT, &bytes)
            .filter(|reference| INPUT_CONTENT.reference(&bytes) == Some(*reference))
            .ok_or_else(|| operational("Run input content exceeds transport admission"))?;
        Ok(Input {
            _charge: None,
            signature,
            owner_signature,
            content,
            specializations,
            dependencies,
            source,
            executable,
            generated,
            implementation,
        })
    }
}

/// Independent of the query owner, so a published handle cannot form a cycle.
#[derive(Clone)]
pub(super) struct ExecutionAdmission {
    remaining: Arc<Mutex<u64>>,
    allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    pub(super) scratch: u64,
}
impl ExecutionAdmission {
    pub(super) fn reserve(&self, bytes: u64) -> Result<Arc<InputCharge>, QueryError> {
        let bytes = bytes
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<InputCharge>::uninit(),
            ))
            .ok_or_else(|| operational("installed decode charge size overflow"))?;
        if bytes > self.scratch {
            return Err(operational(format!(
                "installed decode exceeds scratch admission: {bytes} requested, {} available",
                self.scratch
            )));
        }
        let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        *remaining = remaining.checked_sub(bytes).ok_or_else(|| {
            operational(format!(
                "installed decode exceeds retention admission: {bytes} requested, {remaining} available"
            ))
        })?;
        Ok(Arc::new(InputCharge {
            remaining: self.remaining.clone(),
            _allowance: self.allowance.clone(),
            bytes,
        }))
    }
}

/// The last consumer releases decoded headers and dependencies before
/// refunding their debit. The catalogue itself keeps no decoded allocation.
pub(super) struct InputCharge {
    remaining: Arc<Mutex<u64>>,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    bytes: u64,
}
impl Drop for InputCharge {
    fn drop(&mut self) {
        *self
            .remaining
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += self.bytes;
    }
}
