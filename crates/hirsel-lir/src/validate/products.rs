//! Native-validator-owned portable envelopes. Ordinary content references and
//! producer associations confer no Value/Ready acceptance. Only actual native
//! validation can publish the local association used by this module.
use super::*;
use crate::FnAbiId;
use crate::ir::Callee;
use crate::operation::{OperationArgument, OperationDeclarations};
use crate::products::{CheckedLayouts, LayoutClosure, UnitBody};
use crate::sealed_payload_source::SealSourceWitness;
use hirsel_artifact_store::{Action, Address, Contents, Domain, Reference, Store};
use hirsel_codec::snapshot_struct;
use hirsel_core_contract::{OperationId, OperationRow};
use hirsel_syntax::ContentHash;
use hirsel_syntax::DefPath;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

const RECEIPT: Domain = Domain {
    kind: 0x4300_0001,
    schema: 8,
};
const VALUE_BODY: Domain = Domain {
    kind: 0x4300_0002,
    schema: 2,
};
const READY_BODY: Domain = Domain {
    kind: 0x4300_0003,
    schema: 2,
};
const LAYOUTS: Domain = Domain {
    kind: 0x4300_0004,
    schema: 2,
};

fn body_domain(stage: StageKind) -> Option<Domain> {
    match stage {
        StageKind::Value => Some(VALUE_BODY),
        StageKind::Ready => Some(READY_BODY),
        _ => None,
    }
}
fn address(subject: ContentHash, policy: ContentHash, stage: StageKind) -> Address {
    let kind = match stage {
        StageKind::Value => 0u32,
        StageKind::Ready => 1,
        _ => 2,
    };
    Address(ContentHash::of_parts(
        "hirsel.native.validation-output.v1",
        [
            subject.0.as_slice(),
            policy.0.as_slice(),
            kind.to_le_bytes().as_slice(),
        ],
    ))
}

#[derive(Clone, Debug)]
struct Record {
    subject: ContentHash,
    policy: ContentHash,
    stage: StageKind,
    lir: u32,
    validator: u32,
    target: ContentHash,
    body: ReferenceRow,
    layouts: ReferenceRow,
    definition: DefPath,
    instance: ContentHash,
    implementation_revision: ContentHash,
    source_witness: Option<SealSourceWitness>,
    declarations: super::declaration_reads::Reads,
    linkage: super::linkage::LinkageRecord,
    operations: Vec<OperationRead>,
    manifest: Vec<(OperationId, Option<OperationRow>)>,
    discharge: ContentHash,
    metadata: Vec<u8>,
}
snapshot_struct!(Record {
    subject,
    policy,
    stage,
    lir,
    validator,
    target,
    body,
    layouts,
    definition,
    instance,
    implementation_revision,
    source_witness,
    declarations,
    linkage,
    operations,
    manifest,
    discharge,
    metadata
});

#[derive(Clone, Debug)]
struct OperationRead {
    id: OperationId,
    arguments: Vec<OperationArgument>,
    output: ContentHash,
}
snapshot_struct!(OperationRead {
    id,
    arguments,
    output
});

#[derive(Clone, Debug)]
struct ReferenceRow {
    kind: u32,
    schema: u32,
    output: ContentHash,
    len: u64,
}
snapshot_struct!(ReferenceRow {
    kind,
    schema,
    output,
    len
});
impl From<Reference> for ReferenceRow {
    fn from(r: Reference) -> Self {
        Self {
            kind: r.domain.kind,
            schema: r.domain.schema,
            output: r.output.0,
            len: r.len,
        }
    }
}
impl ReferenceRow {
    fn reference(&self) -> Reference {
        Reference {
            domain: Domain {
                kind: self.kind,
                schema: self.schema,
            },
            output: hirsel_artifact_store::Output(self.output),
            len: self.len,
        }
    }
}

/// Independently supplied current native environment. Reuse reads the exact
/// declaration/manifest/discharge observations extracted by validation itself;
/// a producer's complete action cannot replace these independent checks.
pub struct NativeEnvironment<'a> {
    pub target: &'a TargetSpec,
    pub manifest: &'a Manifest,
    pub operations: &'a (dyn OperationDeclarations + Sync),
    pub discharge: Option<&'a crate::discharge::Discharge>,
}
fn discharge_identity(plan: Option<&crate::discharge::Discharge>, owner: &DefPath) -> ContentHash {
    use crate::discharge::Form;
    let mut parts = Vec::new();
    for site in plan.and_then(|p| p.sites(owner)).into_iter().flatten() {
        parts.push(site.body.to_le_bytes().to_vec());
        parts.push(site.block.to_le_bytes().to_vec());
        parts.push(site.statement.to_le_bytes().to_vec());
        match &site.form {
            Form::Check(rule) => {
                parts.push(vec![0]);
                parts.push(rule.as_bytes().to_vec());
            }
            Form::Integer => parts.push(vec![1]),
            Form::Negation => parts.push(vec![2]),
            Form::Projection { place, rule } => {
                parts.push(vec![3]);
                parts.push(place.to_le_bytes().to_vec());
                parts.push(rule.as_bytes().to_vec());
            }
        }
    }
    ContentHash::of_parts(
        "hirsel.native.discharge-input.v1",
        parts.iter().map(Vec::as_slice),
    )
}
fn operation_identity(
    env: &NativeEnvironment<'_>,
    id: &OperationId,
    args: &[OperationArgument],
) -> Option<ContentHash> {
    let (abi, table) = env.operations.derive(id, args, env.target).ok()?;
    crate::layout_validate::validate_table(&table, env.target).ok()?;
    Some(crate::LayoutKeys::new(&table).fn_abi(&abi))
}

/// Opaque local native acceptance. No Deserialize or producer constructor.
/// Metadata is ordinary query data and never another verifier capability.
#[derive(Clone, Debug)]
pub struct NativeOutput {
    record: Arc<Record>,
}
impl NativeOutput {
    pub fn body(&self) -> Reference {
        self.record.body.reference()
    }
    pub fn layouts(&self) -> Reference {
        self.record.layouts.reference()
    }
    pub fn metadata(&self) -> &[u8] {
        &self.record.metadata
    }
    pub fn stage(&self) -> StageKind {
        self.record.stage
    }
    pub fn linkage(&self) -> super::CheckedLinkage {
        self.record.linkage.checked()
    }
}

/// Failure at a demanded native materialization boundary. The selected
/// provider's refusal survives intact, with the exact required reference.
#[derive(Debug)]
pub enum NativeReadError {
    EnvironmentChanged,
    Layout(String),
    Content {
        reference: Reference,
        reason: hirsel_artifact_store::ContentReadError,
    },
    Decode(String),
}
impl std::fmt::Display for NativeReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvironmentChanged => f.write_str("native output environment changed"),
            Self::Layout(reason) => write!(f, "native layout product: {reason}"),
            Self::Content { reference, reason } => {
                write!(f, "required native product {reference:?}: {reason}")
            }
            Self::Decode(reason) => write!(f, "native body product decode: {reason}"),
        }
    }
}
impl std::error::Error for NativeReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Content { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// Uses the selected ordinary content provider. The only extra table is local
/// native proof authority; it is never a semantic invalidation graph/provider.
type LayoutAdmission = dyn Fn(&CheckedLayouts) -> Option<Arc<dyn Send + Sync>> + Send + Sync;
/// Checked layout tables a keeping validator has admitted, by content
/// reference: a request's validator decodes and checks each table once rather
/// than on every read of it, until an eviction point evicts it.
type LayoutRows = BTreeMap<Reference, Arc<CheckedLayouts>>;
type Evictor = dyn Fn() -> bool + Send + Sync;
/// One kept table's evictable charge. `HIRSEL_NATIVE_RETENTION` prints the
/// kept tables and bytes at each new peak.
struct KeptLayouts(hirsel_guard::admission::MemoryReservation);
static KEPT: Mutex<(u64, u64, u64)> = Mutex::new((0, 0, 0));
impl KeptLayouts {
    fn new(grant: hirsel_guard::admission::MemoryReservation) -> Self {
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        kept.0 += 1;
        kept.1 += grant.bytes();
        if kept.1 > kept.2 {
            kept.2 = kept.1;
            if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
                eprintln!("NATIVE_KEPT_LAYOUTS peak {} tables {}", kept.1, kept.0);
            }
        }
        Self(grant)
    }
}
impl Drop for KeptLayouts {
    fn drop(&mut self) {
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        kept.0 -= 1;
        kept.1 -= self.0.bytes();
    }
}
pub struct NativeCache {
    layouts: Mutex<LayoutRows>,
    layout_admission: Option<Arc<LayoutAdmission>>,
    /// Present only when this validator keeps its tables: the evictor its
    /// budget holds weakly, owned here so it ends with the validator.
    evictor: Option<Arc<Evictor>>,
    contents: Arc<dyn Contents>,
    local: Option<Store>,
    accepted: Mutex<BTreeMap<ContentHash, Arc<Record>>>,
}
impl NativeCache {
    /// The host supplies its explicit own writable Store, never a read-only
    /// packaged/remote root. Without that capability receipts are process-local.
    pub fn new(contents: Arc<dyn Contents>, local: Option<Store>) -> Self {
        Self {
            contents,
            local,
            accepted: Mutex::default(),
            layouts: Mutex::default(),
            layout_admission: None,
            evictor: None,
        }
    }

    /// Keep each checked layout table this validator admits, charged to
    /// `budget` as an evictable charge of `weigh(table)` bytes. The budget's
    /// eviction points and refused reservations evict kept tables no reader
    /// holds; a table a reader holds stays charged until that reader drops it.
    /// A read after an eviction checks and admits the table again, as a first
    /// read does, taking the headroom only once nothing more evicts.
    pub fn keeping_layouts(
        self,
        budget: Arc<hirsel_guard::admission::ProcessBudget>,
        weigh: impl Fn(&CheckedLayouts) -> u64 + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this: &Weak<Self>| {
            let this = this.clone();
            let evictor: Arc<Evictor> =
                Arc::new(move || this.upgrade().is_some_and(|cache| cache.evict_layout()));
            budget.register_evictor(Arc::downgrade(&evictor));
            let mut cache = self.with_layout_admission(move |layouts| {
                let bytes = weigh(layouts);
                let grant = budget
                    .reserve_evictable(bytes)
                    .or_else(|_| budget.reserve_evictable_in_headroom(bytes))
                    .ok()?;
                Some(Arc::new(KeptLayouts::new(grant)) as Arc<dyn Send + Sync>)
            });
            cache.evictor = Some(evictor);
            cache
        })
    }

    /// Drop one kept table no reader holds; false when there is none, or
    /// when a read holds the rows (it is about to use them).
    fn evict_layout(&self) -> bool {
        let Ok(mut rows) = self.layouts.try_lock() else {
            return false;
        };
        let Some(reference) = rows
            .iter()
            .find(|(_, owner)| Arc::strong_count(owner) == 1)
            .map(|(reference, _)| *reference)
        else {
            return false;
        };
        let evicted = rows.remove(&reference);
        drop(rows);
        drop(evicted);
        true
    }

    /// Retaining checked metadata requires the selected transport's original
    /// memory admission. Detached readers keep that reservation through the Arc.
    pub fn with_layout_admission(
        mut self,
        admit: impl Fn(&CheckedLayouts) -> Option<Arc<dyn Send + Sync>> + Send + Sync + 'static,
    ) -> Self {
        self.layout_admission = Some(Arc::new(admit));
        self
    }

    pub fn layouts(
        &self,
        reference: Reference,
        target: &TargetSpec,
    ) -> Result<Arc<CheckedLayouts>, String> {
        self.checked_layouts(reference, target, None)
    }

    fn checked_layouts(
        &self,
        reference: Reference,
        target: &TargetSpec,
        captured: Option<LayoutClosure>,
    ) -> Result<Arc<CheckedLayouts>, String> {
        if reference.domain != LAYOUTS {
            return Err("native layout product schema differs".into());
        }
        // Even a retained typed owner cannot vouch for a changed or missing
        // provider range. Verify the complete reference on every boundary read.
        let bytes = self.get(reference).ok_or("missing native layout product")?;
        let retained = self
            .evictor
            .as_ref()
            .and_then(|_| self.rows().get(&reference).cloned());
        if let Some(owner) = retained {
            if owner.table.target != target.digest() {
                return Err("native layout product target differs".into());
            }
            return Ok(owner);
        }
        let closure = match captured {
            Some(closure) => closure,
            None => {
                hirsel_codec::from_text(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("native layout product: {e:?}"))?
            }
        };
        drop(bytes);
        let mut checked = CheckedLayouts::new(closure, target)?;
        let Some(admit) = &self.layout_admission else {
            return Ok(Arc::new(checked));
        };
        checked.admission =
            Some(admit(&checked).ok_or("native layout metadata exceeds admission")?);
        if self.evictor.is_none() {
            return Ok(Arc::new(checked));
        }
        Ok(self
            .rows()
            .entry(reference)
            .or_insert_with(|| Arc::new(checked))
            .clone())
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, LayoutRows> {
        self.layouts.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, reference: Reference) -> Option<hirsel_artifact_store::Bytes> {
        let bytes = self.contents.get_content(reference).ok()?;
        bytes.matches_reference(reference).then_some(bytes)
    }
    fn put(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let expected = domain.reference(bytes)?;
        if self.contents.put_content(domain, bytes)? != expected {
            return None;
        }
        Some(expected)
    }

    /// Reconstruct acceptance from this validator's own association only.
    /// The caller first validates the query's complete current dependency
    /// outputs, including source/private proof, layouts, graph, policy and all
    /// explicit callee/optimizer inputs, to obtain `subject`.
    ///
    /// `Ok(None)` means no accepted record is current: none was associated,
    /// it differs from the current environment, or its body or layouts are
    /// absent, changed or unreadable. A read refused for admission is an
    /// error, never a miss.
    pub fn lookup(
        &self,
        subject: ContentHash,
        policy: ContentHash,
        stage: StageKind,
        environment: &NativeEnvironment<'_>,
    ) -> Result<Option<NativeOutput>, NativeReadError> {
        let Some(record) = self.accepted_record(subject, policy, stage, environment) else {
            return Ok(None);
        };
        // Value and Ready bodies remain unopened. Structural declaration reads
        // may bind only the portable layout metadata needed for reconstruction.
        if !self.present(record.body.reference())? || !self.present(record.layouts.reference())? {
            return Ok(None);
        }
        if record.declarations.has_structural() {
            let Ok(layouts) = self.layouts(record.layouts.reference(), environment.target) else {
                return Ok(None);
            };
            let binding = layouts.binding();
            if !record.declarations.matches_structural(
                environment.operations,
                &binding.table,
                environment.target,
            ) {
                return Ok(None);
            }
        }
        Ok(Some(NativeOutput { record }))
    }

    /// Whether the provider holds exactly `reference`. Absent, changed or
    /// unreadable content is a miss that a recompute replaces; a read the
    /// request's admission or the provider refused is reported as such.
    fn present(&self, reference: Reference) -> Result<bool, NativeReadError> {
        use hirsel_artifact_store::ContentReadError;
        match self.contents.get_content(reference) {
            Ok(bytes) => Ok(bytes.matches_reference(reference)),
            Err(reason @ (ContentReadError::Admission { .. } | ContentReadError::Refused(_))) => {
                Err(NativeReadError::Content { reference, reason })
            }
            Err(_) => Ok(false),
        }
    }

    /// The accepted record for `subject`, if it matches the current
    /// environment. No product is read.
    fn accepted_record(
        &self,
        subject: ContentHash,
        policy: ContentHash,
        stage: StageKind,
        environment: &NativeEnvironment<'_>,
    ) -> Option<Arc<Record>> {
        let domain = body_domain(stage)?;
        let key = address(subject, policy, stage);
        let record = self
            .accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key.0)
            .cloned()
            .or_else(|| {
                let local = self.local.as_ref()?;
                let association = local.lookup(key)?;
                if association.action != Action(key.0) || association.reference.domain != RECEIPT {
                    return None;
                }
                let bytes = local.get(association.reference)?;
                let record: Record =
                    hirsel_codec::from_text(std::str::from_utf8(&bytes).ok()?).ok()?;
                Some(Arc::new(record))
            })?;
        if record.subject != subject
            || record.policy != policy
            || record.stage != stage
            || record.lir != crate::LIR_VERSION
            || record.validator != crate::VALIDATOR_VERSION
            || record.target != environment.target.digest()
            || record.body.reference().domain != domain
            || record.layouts.reference().domain != LAYOUTS
        {
            return None;
        }
        if record
            .manifest
            .iter()
            .any(|(id, row)| environment.manifest.row(id) != row.as_ref())
            || record.operations.iter().any(|read| {
                operation_identity(environment, &read.id, &read.arguments) != Some(read.output)
            })
            || !record
                .declarations
                .matches(environment.operations, environment.target)
            || record.discharge != discharge_identity(environment.discharge, &record.definition)
            || record.source_witness
                != environment
                    .operations
                    .seal_source_witness(&record.definition, record.instance)
            || record.source_witness.as_ref().is_some_and(|witness| {
                witness.definition != record.definition
                    || witness.instance != record.instance
                    || witness.implementation_revision != record.implementation_revision
            })
        {
            return None;
        }
        Some(record)
    }

    /// Validate actual operations and complete independently supplied inputs,
    /// then choose and publish the exact body/layout envelopes ourselves.
    /// A producer cannot pair its own arbitrary reference with a receipt.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_and_publish<S: Stage>(
        &self,
        subject: ContentHash,
        policy: ContentHash,
        unit: LirUnit<S>,
        inputs: &Inputs<'_>,
        additional_abis: &[FnAbiId],
        metadata: &[u8],
    ) -> Result<(ValidatedUnit<S>, Option<NativeOutput>), Box<Rejected<S>>> {
        let (checked, output) =
            self.validate_output(subject, policy, unit, inputs, additional_abis)?;
        let output = output.and_then(|output| self.publish_output(&output, metadata));
        Ok((checked, output))
    }

    /// Run actual C once and capture its chosen body/layouts and independent
    /// observations. The opaque output is not installed as a successful lookup
    /// association until publish_output, so a query can first derive ordinary
    /// local equations and discovery metadata from the checked unit.
    pub fn validate_output<S: Stage>(
        &self,
        subject: ContentHash,
        policy: ContentHash,
        unit: LirUnit<S>,
        inputs: &Inputs<'_>,
        additional_abis: &[FnAbiId],
    ) -> Result<(ValidatedUnit<S>, Option<NativeOutput>), Box<Rejected<S>>> {
        let observed = super::declaration_reads::Observed::new(inputs.operations);
        let checked_inputs = Inputs {
            target: inputs.target,
            layouts: inputs.layouts,
            manifest: inputs.manifest,
            operations: &observed,
            discharge: inputs.discharge,
            memo: Default::default(),
        };
        let checked = validate(unit, &checked_inputs)?;
        let capture = || -> Option<NativeOutput> {
            let domain = body_domain(S::KIND)?;
            // Observe the independent original-source provider, never candidate
            // metadata. Unavailable is retained as None, not expected-empty.
            let header = &checked.unit().header;
            let source_witness = inputs
                .operations
                .seal_source_witness(&header.definition, header.instance);
            if source_witness.as_ref().is_some_and(|witness| {
                witness.definition != header.definition
                    || witness.instance != header.instance
                    || witness.implementation_revision != header.implementation_revision
            }) {
                return None;
            }
            let (closure, body) = LayoutClosure::capture(&checked, inputs, additional_abis).ok()?;
            let layouts = self.put(LAYOUTS, hirsel_codec::to_text(&closure).as_bytes())?;
            let checked_layouts = self
                .checked_layouts(layouts, inputs.target, Some(closure))
                .ok()?;
            let declarations = observed.finish(
                &checked_layouts.binding().table,
                inputs.layouts,
                inputs.target,
            )?;
            let body = self.put(domain, hirsel_codec::to_text(&body).as_bytes())?;
            let environment = NativeEnvironment {
                target: inputs.target,
                manifest: inputs.manifest,
                operations: inputs.operations,
                discharge: inputs.discharge,
            };
            let mut operation_reads = BTreeMap::new();
            for call in &checked.unit().tables.calls {
                if let Callee::Operation(op) = call.callee {
                    let id = &checked.unit().tables.operations[op.index()];
                    let hash = operation_identity(&environment, id, &call.operation_arguments)?;
                    let encoding =
                        hirsel_codec::to_text(&(id.clone(), call.operation_arguments.clone()));
                    operation_reads.insert(
                        encoding,
                        OperationRead {
                            id: id.clone(),
                            arguments: call.operation_arguments.clone(),
                            output: hash,
                        },
                    );
                }
            }
            let record = Arc::new(Record {
                subject,
                policy,
                stage: S::KIND,
                lir: crate::LIR_VERSION,
                validator: crate::VALIDATOR_VERSION,
                target: inputs.target.digest(),
                body: body.into(),
                layouts: layouts.into(),
                definition: header.definition.clone(),
                instance: header.instance,
                implementation_revision: header.implementation_revision,
                source_witness,
                declarations,
                linkage: super::linkage::LinkageRecord::capture(&checked, inputs.layouts)?,
                operations: operation_reads.into_values().collect(),
                manifest: checked
                    .unit()
                    .tables
                    .operations
                    .iter()
                    .map(|id| (id.clone(), inputs.manifest.row(id).cloned()))
                    .collect(),
                discharge: discharge_identity(inputs.discharge, &checked.unit().header.definition),
                metadata: Vec::new(),
            });
            Some(NativeOutput { record })
        };
        let output = capture();
        Ok((checked, output))
    }

    /// Attach ordinary query metadata to an actual C-issued output, retaining
    /// every independent observation verbatim. No body, target, policy, stage,
    /// operation, source witness or discharge evidence can be replaced here.
    /// Lookup still rechecks the current independent environment.
    pub fn publish_output(&self, output: &NativeOutput, metadata: &[u8]) -> Option<NativeOutput> {
        if output.record.lir != crate::LIR_VERSION
            || output.record.validator != crate::VALIDATOR_VERSION
            || body_domain(output.stage()) != Some(output.body().domain)
            || output.layouts().domain != LAYOUTS
        {
            return None;
        }
        self.get(output.body())?;
        self.get(output.layouts())?;
        let mut record = (*output.record).clone();
        record.metadata = metadata.to_vec();
        let record = Arc::new(record);
        let key = address(record.subject, record.policy, record.stage);
        if let Some(local) = &self.local {
            let bytes = hirsel_codec::to_text(record.as_ref());
            if let Ok(reference) = local.put(RECEIPT, bytes.as_bytes()) {
                let _ = local.associate(key, Action(key.0), reference);
            }
        }
        self.accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.0, record.clone());
        Some(NativeOutput { record })
    }

    /// Obtain portable materialization inputs from an already accepted output.
    /// Native stage validation still seals the actual temporary bound unit.
    pub fn read(
        &self,
        output: &NativeOutput,
        target: &TargetSpec,
    ) -> Result<(Arc<CheckedLayouts>, UnitBody), NativeReadError> {
        if output.record.target != target.digest()
            || output.record.lir != crate::LIR_VERSION
            || output.record.validator != crate::VALIDATOR_VERSION
        {
            return Err(NativeReadError::EnvironmentChanged);
        }
        let closure = self
            .layouts(output.layouts(), target)
            .map_err(NativeReadError::Layout)?;
        let body_error = |reason| NativeReadError::Content {
            reference: output.body(),
            reason,
        };
        let body = self
            .contents
            .get_content(output.body())
            .map_err(body_error)?;
        if !body.matches_reference(output.body()) {
            return Err(body_error(
                hirsel_artifact_store::ContentReadError::InvalidContent,
            ));
        }
        let body: UnitBody = hirsel_codec::from_text(
            std::str::from_utf8(&body).map_err(|e| NativeReadError::Decode(e.to_string()))?,
        )
        .map_err(|e| NativeReadError::Decode(format!("{e:?}")))?;
        Ok((closure, body))
    }

    /// A self-contained local materialization for a demanded transformation.
    pub fn materialize<S: Stage>(
        &self,
        output: &NativeOutput,
        inputs: &Inputs<'_>,
    ) -> Result<ValidatedUnit<S>, String> {
        if output.stage() != S::KIND {
            return Err("native output stage differs".into());
        }
        let (closure, body) = self
            .read(output, inputs.target)
            .map_err(|e| e.to_string())?;
        let binding = closure.binding();
        if &binding.table != inputs.layouts {
            return Err("native materialization needs its bound local table".into());
        }
        let unit = binding.materialize::<S>(&body)?;
        validate(unit, inputs).map_err(|e| format!("native output refused: {:?}", e.violations))
    }
}
