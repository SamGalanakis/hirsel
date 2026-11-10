//! Durable source accepted execution. Root membership and local acceptance are
//! separate from structural decoding and the ordinary V gate.
use super::*;
use crate::NativeRecordSource;
use hirsel_artifact_store::{Action, Address, Store};
use hirsel_product_codec::{TypeRecords, decode_schema::Walk};
use hirsel_tir::{DefinitionRef, MaterializedRef, TirProvider};
use hirsel_types::{GenericArgument, TypeInterner};
use serde::{Deserialize, Serialize};

pub trait ExecutionProducts: Send + Sync {
    fn definition(&self, path: &DefPath) -> Result<DefinitionRef, QueryError>;
    fn constant(&self, path: &DefPath, actuals: &[u64]) -> Result<MaterializedRef, QueryError>;
    fn constant_definitions(&self) -> Result<Vec<DefPath>, QueryError>;
    fn instance_body(
        &self,
        path: &DefPath,
        actuals: &[GenericArgument],
    ) -> Result<Option<DefPath>, QueryError>;
}
#[derive(Clone)]
pub struct AcceptedExecutionProduct {
    pub entry: DefPath,
    pub definition: DefinitionRef,
    pub signature: SignatureRef,
    pub sources: SourceMap,
    pub runtime: Arc<hirsel_interp::aggregate::RuntimeTypes>,
    pub checked_runtime: Arc<hirsel_interp::checked::CheckedRuntime>,
    pub code: hirsel_run_schema::CodeReach,
    pub callees: std::collections::BTreeMap<String, hirsel_run_schema::CalleeFacts>,
    pub entries: std::collections::BTreeMap<
        String,
        (hirsel_run_schema::CalleeFacts, hirsel_run_schema::CallForm),
    >,
    pub revision: hirsel_run_schema::ProgramRevision,
    pub foreign_unreplayable: bool,
    pub foreign: crate::ForeignUse,
    #[doc(hidden)]
    pub products: Arc<dyn ExecutionProducts>,
}
impl AcceptedExecutionProduct {
    pub fn provider(&self) -> impl TirProvider + '_ {
        ExecutionProvider(self.products.as_ref())
    }
}
struct ExecutionProvider<'a>(&'a dyn ExecutionProducts);
impl TirProvider for ExecutionProvider<'_> {
    fn definition(&mut self, path: &DefPath) -> Result<DefinitionRef, QueryError> {
        self.0.definition(path)
    }
    fn constant(&mut self, path: &DefPath) -> Result<MaterializedRef, QueryError> {
        self.0.constant(path, &[])
    }
    fn constant_instance(
        &mut self,
        path: &DefPath,
        actuals: &[u64],
    ) -> Result<MaterializedRef, QueryError> {
        self.0.constant(path, actuals)
    }
    fn constant_definitions(&mut self) -> Result<Vec<DefPath>, QueryError> {
        self.0.constant_definitions()
    }
    fn instance_body(
        &mut self,
        path: &DefPath,
        actuals: &[GenericArgument],
    ) -> Result<Option<DefPath>, QueryError> {
        self.0.instance_body(path, actuals)
    }
}

const ROOT: Domain = Domain {
    kind: 0x4603_0001,
    schema: 1,
};
const HEADER: Domain = Domain {
    kind: 0x4603_0002,
    schema: 2,
};
const CONSTANT: Domain = Domain {
    kind: 0x4603_0003,
    schema: 1,
};
const SOURCE: Domain = Domain {
    kind: 0x4603_0004,
    schema: 1,
};
const LOCATOR: Domain = Domain {
    kind: 0x4603_0006,
    schema: 1,
};
#[derive(Serialize, Deserialize)]
struct Locator {
    namespace: [u8; 32],
    observations: [u8; 32],
    imports: [u8; 32],
    compiler: [u8; 32],
    policy: [u8; 32],
    options: [u8; 32],
    entry: DefPath,
    root: Reference,
}
hirsel_product_codec::decode_structure!(Locator {
    namespace,
    observations,
    imports,
    compiler,
    policy,
    options,
    entry,
    root
});
impl Locator {
    fn inputs(&self) -> hirsel_validate::ExecutionProductInputs {
        hirsel_validate::ExecutionProductInputs {
            namespace: ContentHash(self.namespace),
            observations: ContentHash(self.observations),
            imports: ContentHash(self.imports),
            compiler: ContentHash(self.compiler),
            policy: ContentHash(self.policy),
            options: ContentHash(self.options),
            entry: self.entry.clone(),
        }
    }
}
type Reach = Option<Vec<String>>;
type Callee = ([u8; 32], [u8; 32], [u8; 32], Reach);
type Node = (String, Vec<Vec<u8>>, Vec<String>);
#[derive(Serialize, Deserialize)]
struct Row {
    path: DefPath,
    builtin: bool,
    header: Reference,
    body: Option<Reference>,
    constant: Option<Reference>,
}
hirsel_product_codec::decode_structure!(Row {
    path,
    builtin,
    header,
    body,
    constant
});
#[derive(Serialize, Deserialize)]
struct SourceFile {
    path: String,
    logical: String,
    text: Reference,
    anchors: Vec<(DefPath, u32, u32)>,
}
hirsel_product_codec::decode_structure!(SourceFile {
    path,
    logical,
    text,
    anchors
});
#[derive(Serialize, Deserialize)]
struct Product {
    // The namespace is the original admitted capture, restored only through
    // the verifier's local source acceptance after independent observation.
    namespace: [u8; 32],
    observations: [u8; 32],
    imports: [u8; 32],
    compiler: [u8; 32],
    policy: [u8; 32],
    options: [u8; 32],
    entry: DefPath,
    rows: Vec<Row>,
    constants: Vec<DefPath>,
    instances: Vec<(DefPath, Vec<(Vec<GenericArgument>, DefPath)>)>,
    sources: Vec<SourceFile>,
    nodes: Vec<Node>,
    code: Reach,
    callees: BTreeMap<String, Callee>,
    entries: BTreeMap<String, (Callee, bool)>,
    foreign_unreplayable: bool,
    externs: BTreeSet<DefPath>,
    unmodeled: BTreeMap<DefPath, DefPath>,
}
hirsel_product_codec::decode_structure!(Product {
    namespace,
    observations,
    imports,
    compiler,
    policy,
    options,
    entry,
    rows,
    constants,
    instances,
    sources,
    nodes,
    code,
    callees,
    entries,
    foreign_unreplayable,
    externs,
    unmodeled
});
impl Product {
    fn inputs(&self) -> hirsel_validate::ExecutionProductInputs {
        hirsel_validate::ExecutionProductInputs {
            namespace: ContentHash(self.namespace),
            observations: ContentHash(self.observations),
            imports: ContentHash(self.imports),
            compiler: ContentHash(self.compiler),
            policy: ContentHash(self.policy),
            options: ContentHash(self.options),
            entry: self.entry.clone(),
        }
    }
}
fn reach(value: &CodeReach) -> Reach {
    match value {
        CodeReach::Safe => None,
        CodeReach::Escapes(paths) => Some(paths.clone()),
    }
}
fn code(value: Reach) -> CodeReach {
    value.map_or(CodeReach::Safe, CodeReach::Escapes)
}
fn callee(value: &CalleeFacts) -> Callee {
    (
        value.program,
        value.code,
        value.fingerprint,
        reach(&value.reach),
    )
}
fn facts(value: Callee) -> CalleeFacts {
    CalleeFacts {
        program: value.0,
        code: value.1,
        fingerprint: value.2,
        reach: code(value.3),
    }
}
fn failure(message: impl Into<String>) -> QueryError {
    operational(format!("accepted execution: {}", message.into()))
}
fn durable_store() -> Result<Store, QueryError> {
    // The standard library also consults the account database on Unix when
    // sandboxed launchers omit HOME. Query caches and temporary directories
    // never own this retained data.
    #[allow(deprecated)]
    let home = std::env::home_dir();
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|home| home.join(".local/share")))
        .ok_or_else(|| failure("durable data directory unavailable"))?;
    if !base.is_absolute() {
        return Err(failure("durable data directory is relative"));
    }
    Ok(Store::new(base.join("hirsel/accepted-execution/v1")))
}
fn index(revision: &[u8; 32], entry: &DefPath) -> Address {
    Address(ContentHash::of_parts(
        "hirsel.accepted-execution.index.v1",
        [revision.as_slice(), entry.to_string().as_bytes()],
    ))
}
fn input_index(
    entry: &DefPath,
    observations: ContentHash,
    imports: ContentHash,
    compiler: ContentHash,
    policy: ContentHash,
    options: ContentHash,
) -> Address {
    Address(ContentHash::of_parts(
        "hirsel.accepted-execution.inputs.v2",
        [
            entry.to_string().as_bytes(),
            &observations.0,
            &imports.0,
            &compiler.0,
            &policy.0,
            &options.0,
        ],
    ))
}
fn revision_input_index(revision: &[u8; 32], inputs: Address) -> Address {
    Address(ContentHash::of_parts(
        "hirsel.accepted-execution.revision-inputs.v1",
        [revision.as_slice(), &inputs.0.0],
    ))
}
fn profile(start: std::time::Instant, phase: &str) {
    crate::queries::operation_owner_trace(format_args!(
        "accepted-execution phase={phase} elapsed_ns={}",
        start.elapsed().as_nanos()
    ));
}
fn put(store: &Store, domain: Domain, bytes: &[u8]) -> Result<Reference, QueryError> {
    store
        .put(domain, bytes)
        .map_err(|error| failure(error.to_string()))
}
fn get(store: &Store, reference: Reference) -> Result<hirsel_artifact_store::Bytes, QueryError> {
    store
        .get_owned(reference)
        .map_err(|error| failure(format!("content unavailable: {error:?}")))
}
struct Bindings {
    types: Arc<TypeInterner>,
    owners: BTreeMap<hirsel_span::Anchor, (DefPath, u32)>,
    anchors: BTreeMap<(DefPath, u32), hirsel_span::Anchor>,
}
impl NativeRecordSource for Bindings {
    fn record_types(&self) -> &Arc<TypeInterner> {
        &self.types
    }
    fn record_owner(&self, anchor: hirsel_span::Anchor) -> Option<(DefPath, u32)> {
        self.owners.get(&anchor).cloned()
    }
    fn record_source_anchor(&self, path: &DefPath, occurrence: u32) -> Option<hirsel_span::Anchor> {
        self.anchors.get(&(path.clone(), occurrence)).copied()
    }
}
type ExecutionRetention =
    Arc<dyn Fn(u64) -> Result<Arc<dyn Send + Sync>, QueryError> + Send + Sync>;
struct BodyImports {
    definitions: BTreeMap<DefPath, DefinitionRef>,
    owners: Vec<Arc<dyn Send + Sync>>,
    environments: BTreeMap<hirsel_syntax::PackageId, hirsel_validate::PackageViews>,
    validations: hirsel_validate::ValidatedSignatures,
}
struct Products {
    types: Arc<TypeInterner>,
    bodies: BTreeMap<DefPath, Reference>,
    imports: Mutex<BodyImports>,
    records: Arc<TypeRecords>,
    anchors: Bindings,
    public: Arc<hirsel_validate::ViewTable>,
    private: BTreeMap<DefPath, SignatureRef>,
    store: Store,
    inputs: hirsel_validate::ExecutionProductInputs,
    limit: u64,
    retain: ExecutionRetention,
    constants: BTreeMap<DefPath, MaterializedRef>,
    order: Vec<DefPath>,
    instances: hirsel_tir::InstanceBodies,
    _admission: Arc<dyn Send + Sync>,
}
impl ExecutionProducts for Products {
    fn definition(&self, path: &DefPath) -> Result<DefinitionRef, QueryError> {
        let mut imports = self.imports.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(definition) = imports.definitions.get(path) {
            return Ok(definition.clone());
        }
        let reference = *self
            .bodies
            .get(path)
            .ok_or_else(|| failure(format!("missing body {path}")))?;
        let bytes = get(&self.store, reference)?;
        let needed = hirsel_product_codec::tir_decode_bytes(&bytes)
            .ok_or_else(|| failure("invalid TIR layout"))?;
        if needed > self.limit {
            return Err(failure("TIR exceeds admission"));
        }
        let decode_owner = (self.retain)(needed)?;
        let archive = Arc::new(
            hirsel_product_codec::CheckedTir::open(
                bytes,
                reference,
                self.records.clone(),
                |path, occurrence| self.anchors.record_source_anchor(path, occurrence),
            )
            .ok_or_else(|| failure("invalid TIR"))?,
        );
        let package = &path.0.package;
        if !imports.environments.contains_key(package) {
            let replacements = self
                .private
                .iter()
                .filter(|(path, _)| &path.0.package == package)
                .map(|(_, sig)| hirsel_validate::View::new(&self.types, sig.clone(), None));
            let views = hirsel_validate::PackageViews::new(
                &self.types,
                self.public.clone(),
                replacements,
                &mut imports.validations,
                None,
            )
            .map_err(|e| failure(format!("private declarations: {e:?}")))?;
            imports.environments.insert(package.clone(), views);
        }
        let verifier = hirsel_validate::VerifierCache::directory_store(self.store.clone());
        let verified = verifier
            .import_execution_definition(
                &self.inputs,
                &archive,
                self.types.clone(),
                |anchor| self.anchors.record_owner(anchor),
                &imports.environments[package],
            )
            .map_err(|e| failure(format!("V refusal: {e:?}")))?;
        if verified.is_none() {
            return Err(failure(format!("V rejected {path}")));
        }
        drop(verified);
        let definition = archive
            .materialize()
            .ok_or_else(|| failure("invalid TIR payload"))?;
        if definition.path.path() != path {
            return Err(failure("body membership"));
        }
        let retained = hirsel_query_products::retained::decoded_tir_bytes(&definition);
        drop(archive);
        drop(decode_owner);
        imports.owners.push((self.retain)(retained)?);
        crate::queries::operation_owner_trace(format_args!(
            "accepted-execution demanded body={path} peak={needed} retained={retained}"
        ));
        let definition = Arc::new(definition);
        imports.definitions.insert(path.clone(), definition.clone());
        Ok(definition)
    }
    fn constant(&self, path: &DefPath, actuals: &[u64]) -> Result<MaterializedRef, QueryError> {
        let selected = if actuals.is_empty() {
            path.clone()
        } else {
            hirsel_tir::constant_instance_path(path, actuals)
        };
        self.constants
            .get(&selected)
            .cloned()
            .ok_or_else(|| failure(format!("missing constant {selected}")))
    }
    fn constant_definitions(&self) -> Result<Vec<DefPath>, QueryError> {
        Ok(self.order.clone())
    }
    fn instance_body(
        &self,
        path: &DefPath,
        actuals: &[GenericArgument],
    ) -> Result<Option<DefPath>, QueryError> {
        self.instances
            .resolve(DefId::intern(path), &self.types, actuals)
            .map(|id| id.map(|id| id.path().clone()))
    }
}

/// Native consumers retain only this durable locator. Interpreter consumers
/// load through the same importer as replay on their first demand.
pub(super) struct PublishedExecution {
    directory: std::path::PathBuf,
    root: Reference,
    inputs: hirsel_validate::ExecutionProductInputs,
    revision: [u8; 32],
    admission: inputs::ExecutionAdmission,
    loaded: Mutex<std::sync::Weak<AcceptedExecutionProduct>>,
    _charge: Arc<inputs::InputCharge>,
}
impl FrontRunOutput {
    pub fn accepted_execution(&self) -> Result<Arc<AcceptedExecutionProduct>, QueryError> {
        let published = self
            .accepted
            .get()
            .ok_or_else(|| failure("product not published"))?;
        let mut loaded = published.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(execution) = loaded.upgrade() {
            return Ok(execution);
        }
        let admission = published.admission.clone();
        let execution = load(
            &Store::new(published.directory.clone()),
            published.root,
            &published.inputs,
            published.admission.scratch,
            move |bytes| {
                admission
                    .reserve(bytes)
                    .map(|owner| owner as Arc<dyn Send + Sync>)
            },
        )?;
        if execution.revision.digest() != published.revision {
            return Err(failure("semantic revision differs at publication"));
        }
        let execution = Arc::new(execution);
        *loaded = Arc::downgrade(&execution);
        Ok(execution)
    }
}
impl FrontRunQueries {
    // Both source-check joins call this with the same admitted operation.
    pub(crate) fn publish_execution(
        &self,
        output: &FrontRunOutput,
        checked: &ProductRef,
    ) -> Result<(), QueryError> {
        if output.accepted.get().is_some() {
            return Ok(());
        }
        let Some(identity) = self.execution_identity.as_ref() else {
            return Ok(());
        };
        let namespace = self
            .operation
            .admitted_identity()
            .ok_or_else(|| failure("unadmitted capture"))?;
        let store = durable_store()?;
        // Strict fsync alone still permits derivable index publications to be
        // skipped. Retain the entire closure and propagate every refusal.
        let _retention = store
            .retention_scope()
            .map_err(|error| failure(error.to_string()))?;
        let batch = store.batch();
        let types = self.lower.signatures().types().clone();
        let records = TypeRecords::new(types.clone(), Arc::new(store.clone()));
        let snapshot = self.lower.signatures();
        let builtins: BTreeSet<_> = snapshot.execution_declarations().into_iter().collect();
        let mut paths = builtins.clone();
        paths.extend(output.semantic.definitions().iter().cloned());
        if let Some(headers) = snapshot.header_products() {
            paths.extend(headers.staged_constants().keys().cloned());
        }
        let constants = self.lower.settled_constants();
        paths.extend(constants.iter().cloned());
        let mut rows = Vec::new();
        let mut source_anchors = BTreeSet::new();
        let mut bind_anchor = |anchor| {
            let owner = self.anchors.record_owner(anchor)?;
            source_anchors.insert(owner.clone());
            Some(owner)
        };
        let mut published = BTreeSet::new();
        while let Some(path) = paths.pop_first() {
            if !published.insert(path.clone()) {
                continue;
            }
            let (public, private) = match self.lower.staged_instance_signature(&path)? {
                Some(signature) => (signature.clone(), signature),
                None => snapshot.execution_declaration(&path)?,
            };
            let mut type_roots = hirsel_types::signature_types(&types, &public);
            type_roots.extend(hirsel_types::signature_types(&types, &private));
            // V checks declaration protocols even when their operations have
            // no executable caller. Retain those signatures with the type.
            for signature in [&public, &private] {
                if let SignatureKind::Type(declaration) = &signature.kind {
                    if let Some(association) = &declaration.loop_source {
                        paths.extend([
                            association.open.clone(),
                            association.next.clone(),
                            association.option.clone(),
                        ]);
                    }
                    paths.extend(
                        declaration
                            .format
                            .iter()
                            .chain(declaration.drop.iter())
                            .cloned(),
                    );
                    if let Some(check) = &declaration.serializable_check {
                        paths.insert(check.function.clone());
                    }
                }
            }
            let private = (!Arc::ptr_eq(&public, &private)).then_some(private);
            let materialized = constants.contains(&path)
                || (output.semantic.definitions().contains(&path)
                    && matches!(public.kind, SignatureKind::Const { .. }));
            let header = records
                .encode_record(&(public, private), &mut bind_anchor)
                .ok_or_else(|| failure(format!("declaration encoding {path}")))?;
            let _header_charge = self.admit_decode(header.len() as u64)?;
            let header = put(&store, HEADER, &header)?;
            let executable = output.semantic.definitions().contains(&path)
                && self
                    .semantic_ids
                    .get(&path)
                    .and_then(|query| {
                        self.semantic_completed
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&query.address.0)
                            .map(|(_, entry)| entry.content.has_body())
                    })
                    .unwrap_or(false);
            let body = if executable {
                let definition = self.lower.settled_definition(&path)?;
                type_roots.extend(definition.types.iter().copied());
                let bytes = records
                    .encode_tir(&definition, &mut bind_anchor)
                    .ok_or_else(|| failure(format!("body encoding {path}")))?;
                let _charge = self.admit_decode(bytes.len() as u64)?;
                Some(put(
                    &store,
                    hirsel_product_codec::CheckedTir::DOMAIN,
                    &bytes,
                )?)
            } else {
                None
            };
            let constant = if materialized {
                let value = self.lower.settled_constant(&path, &[])?;
                let bytes = records
                    .encode_record(&value, &mut bind_anchor)
                    .ok_or_else(|| failure(format!("constant encoding {path}")))?;
                let _charge = self.admit_decode(bytes.len() as u64)?;
                Some(put(&store, CONSTANT, &bytes)?)
            } else {
                None
            };
            for id in hirsel_types::closure(&types, type_roots).iter() {
                let ty = types
                    .get(*id)
                    .ok_or_else(|| failure("missing runtime type row"))?;
                if let hirsel_types::TypeKind::Named { definition, .. } = &ty.kind
                    && !published.contains(definition.path())
                {
                    paths.insert(definition.path().clone());
                }
            }
            rows.push(Row {
                builtin: builtins.contains(&path),
                path,
                header,
                body,
                constant,
            });
        }
        let mut files: BTreeMap<hirsel_span::FileId, Vec<(DefPath, u32, u32)>> = BTreeMap::new();
        for (anchor, file, base) in self.lower.sources().anchors() {
            if let Some((owner, occurrence)) = self.anchors.record_owner(anchor)
                && source_anchors.contains(&(owner.clone(), occurrence))
                && self.anchors.record_source_anchor(&owner, occurrence) == Some(anchor)
            {
                files
                    .entry(file)
                    .or_default()
                    .push((owner, occurrence, base));
            }
        }
        let mut sources = Vec::new();
        for (file, anchors) in files {
            let file = self
                .lower
                .sources()
                .file(file)
                .ok_or_else(|| failure("missing source"))?;
            sources.push(SourceFile {
                path: file
                    .path
                    .to_str()
                    .ok_or_else(|| failure("source path encoding"))?
                    .to_owned(),
                logical: file
                    .logical_path()
                    .to_str()
                    .ok_or_else(|| failure("logical source path encoding"))?
                    .to_owned(),
                text: put(&store, SOURCE, file.text().as_bytes())?,
                anchors,
            });
        }
        let mut instances = Vec::new();
        for declaration in &self.operation.capture().instances.declarations {
            let values = self
                .operation
                .capture()
                .instances
                .instances
                .get(declaration)
                .into_iter()
                .flat_map(|values| values.values())
                .map(|value| (value.arguments.clone(), value.definition.path().clone()))
                .collect();
            instances.push((declaration.path().clone(), values));
        }
        let nodes = output
            .semantic
            .execution_nodes()
            .map(|node| {
                (
                    node.identity.clone(),
                    node.content.clone(),
                    node.dependencies.clone(),
                )
            })
            .collect();
        let product = Product {
            namespace: namespace.0,
            observations: identity.observations.0,
            imports: self.operation.capture().installation.0,
            compiler: identity.compiler.0,
            policy: snapshot.source_verification_policy().0,
            options: identity.options.0,
            entry: output.entry.clone(),
            rows,
            constants,
            instances,
            sources,
            nodes,
            code: reach(&output.code),
            callees: output
                .callees
                .iter()
                .map(|(path, value)| (path.clone(), callee(value)))
                .collect(),
            entries: output
                .entries
                .iter()
                .map(|(path, (value, form))| {
                    (path.clone(), (callee(value), *form == CallForm::Call))
                })
                .collect(),
            foreign_unreplayable: output.semantic.foreign_unreplayable(),
            externs: output.foreign.externs.clone(),
            unmodeled: output.foreign.unmodeled.clone(),
        };
        crate::queries::operation_owner_trace(format_args!(
            "accepted-execution publish rows={} bodies={} constants={} sources={}",
            product.rows.len(),
            product.rows.iter().filter(|r| r.body.is_some()).count(),
            product.constants.len(),
            product.sources.len()
        ));
        let bytes = records
            .encode_record(&product, |anchor| self.anchors.record_owner(anchor))
            .ok_or_else(|| failure("root encoding"))?;
        let _charge = self.admit_decode(bytes.len() as u64)?;
        let root = put(&store, ROOT, &bytes)?;
        // The accepted live Check action attests this join. Archiving its
        // entire diagnostic graph is independent of executable acceptance.
        let check_bytes = bincode::serialize(&(checked.address.0, checked.action.0, namespace))
            .map_err(|e| failure(e.to_string()))?;
        let source_check = put(
            &store,
            Domain {
                kind: 0x4603_0005,
                schema: 1,
            },
            &check_bytes,
        )?;
        let verifier = hirsel_validate::VerifierCache::directory_store(store.clone());
        verifier
            .publish_execution_product_durable(&product.inputs(), root, source_check)
            .map_err(|e| failure(e.to_string()))?;
        for body in product.rows.iter().filter_map(|row| row.body) {
            verifier
                .publish_execution_product_durable(&product.inputs(), body, source_check)
                .map_err(|e| failure(e.to_string()))?;
        }
        let locator = Locator {
            namespace: product.namespace,
            observations: product.observations,
            imports: product.imports,
            compiler: product.compiler,
            policy: product.policy,
            options: product.options,
            entry: product.entry.clone(),
            root,
        };
        let locator = put(
            &store,
            LOCATOR,
            &bincode::serialize(&locator).map_err(|e| failure(e.to_string()))?,
        )?;
        let address = input_index(
            &output.entry,
            identity.observations,
            self.operation.capture().installation,
            identity.compiler,
            snapshot.source_verification_policy(),
            identity.options,
        );
        store
            .associate(address, Action(address.0), locator)
            .map_err(|e| failure(e.to_string()))?;
        let address = revision_input_index(&output.revision.digest(), address);
        store
            .associate(address, Action(address.0), locator)
            .map_err(|e| failure(e.to_string()))?;
        let address = index(&output.revision.digest(), &output.entry);
        store
            .associate(address, Action(address.0), locator)
            .map_err(|e| failure(e.to_string()))?;
        batch.finish().map_err(|e| failure(e.to_string()))?;
        let inputs = product.inputs();
        let directory = store.root().to_path_buf();
        // Publication and encoding are scratch. Release them before charging
        // the only allocation retained by native Run: its durable handle.
        drop(bytes);
        drop(_charge);
        drop(product);
        drop(records);
        drop(verifier);
        drop(_retention);
        drop(store);
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<PublishedExecution>::uninit(),
            ) as usize,
        );
        weight.add(directory.capacity());
        weight.path(&inputs.entry);
        let charge = self.admit_decode(weight.bytes())?;
        crate::queries::operation_owner_trace(format_args!(
            "accepted-execution retained handle_bytes={}",
            weight.bytes()
        ));
        let _ = output.accepted.set(Arc::new(PublishedExecution {
            directory,
            root,
            inputs,
            revision: output.revision.digest(),
            admission: self.execution_admission(),
            loaded: Mutex::new(std::sync::Weak::new()),
            _charge: charge,
        }));
        Ok(())
    }
}

struct RuntimeRows {
    builtins: BTreeSet<DefPath>,
    products: Arc<Products>,
}
impl hirsel_interp::aggregate::TypeRows for RuntimeRows {
    fn view(
        &self,
        path: &DefPath,
    ) -> Result<Option<(hirsel_interp::aggregate::SerializedView, ContentHash)>, QueryError> {
        Ok(self
            .products
            .private
            .get(path)
            .cloned()
            .map(|signature| (signature.into(), ContentHash::default())))
    }
    fn finalizer(
        &self,
        path: &DefPath,
    ) -> Result<Option<(DefPath, hirsel_tir::BodyId)>, QueryError> {
        if self.products.bodies.contains_key(path) {
            Ok(Some((path.clone(), self.products.definition(path)?.entry)))
        } else if self.builtins.contains(path) {
            Ok(None)
        } else {
            Err(failure(format!("missing finalizer {path}")))
        }
    }
}
fn decode<T: Walk + serde::de::DeserializeOwned>(
    records: &Arc<TypeRecords>,
    bytes: hirsel_artifact_store::Bytes,
    anchors: &Bindings,
    reserve: &dyn Fn(u64) -> Result<Arc<dyn Send + Sync>, QueryError>,
    owners: &mut Vec<Arc<dyn Send + Sync>>,
) -> Result<T, QueryError> {
    let backing = bytes.len();
    let requirement = records
        .record_requirement::<T>(bytes)
        .ok_or_else(|| failure("invalid record"))?;
    let required = requirement.required_bytes();
    let retained = requirement.retained_bytes();
    crate::queries::operation_owner_trace(format_args!(
        "accepted-execution record={} backing={backing} peak={required} retained={retained}",
        std::any::type_name::<T>()
    ));
    let owner = reserve(required)?;
    let value = requirement
        .decode_reserved(
            |_| Some(()),
            |path, occurrence| anchors.record_source_anchor(path, occurrence),
        )
        .ok_or_else(|| failure("record decode refused"))?;
    // decode_reserved consumed its backing and temporary binding tables.
    // Keep its semantic allocations charged, rather than retaining the peak.
    drop(owner);
    owners.push(reserve(retained)?);
    Ok(value)
}
fn load(
    store: &Store,
    root: Reference,
    current: &hirsel_validate::ExecutionProductInputs,
    limit: u64,
    reserve: impl Fn(u64) -> Result<Arc<dyn Send + Sync>, QueryError> + Send + Sync + 'static,
) -> Result<AcceptedExecutionProduct, QueryError> {
    let reserve: ExecutionRetention = Arc::new(reserve);
    let started = std::time::Instant::now();
    profile(started, "import start");
    let verifier = hirsel_validate::VerifierCache::directory_store(store.clone());
    if root.domain != ROOT || verifier.execution_product(current, root).is_none() {
        return Err(failure("missing trusted source binding"));
    }
    let types = TypeInterner::new(None);
    let records = TypeRecords::new(types.clone(), Arc::new(store.clone()));
    let mut anchors = Bindings {
        types: types.clone(),
        owners: BTreeMap::new(),
        anchors: BTreeMap::new(),
    };
    let mut owners = Vec::new();
    let product: Product = decode(
        &records,
        get(store, root)?,
        &anchors,
        &*reserve,
        &mut owners,
    )?;
    profile(started, "root decoded");
    if product.inputs() != *current {
        return Err(failure("input identity differs"));
    }
    let mut sources = SourceMap::default();
    for source in product.sources {
        if source.text.domain != SOURCE {
            return Err(failure("source domain"));
        }
        let bytes = get(store, source.text)?;
        owners.push(reserve(bytes.len() as u64)?);
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| failure("invalid source text"))?;
        let file = sources
            .add_logical(&source.path, &source.logical, text)
            .ok_or_else(|| failure("source size"))?;
        for (path, occurrence, base) in source.anchors {
            let anchor = hirsel_span::Anchor::fresh();
            if anchors
                .anchors
                .insert((path.clone(), occurrence), anchor)
                .is_some()
            {
                return Err(failure("duplicate source binding"));
            }
            anchors.owners.insert(anchor, (path, occurrence));
            sources.move_anchor(anchor, file, base);
        }
    }
    let mut public = Vec::new();
    let mut private = BTreeMap::new();
    let mut header_owners = Vec::new();
    for row in &product.rows {
        if row.header.domain != HEADER {
            return Err(failure("declaration domain"));
        }
        let (exported, owner): (SignatureRef, Option<SignatureRef>) = decode(
            &records,
            get(store, row.header)?,
            &anchors,
            &*reserve,
            &mut header_owners,
        )?;
        let owner = owner.unwrap_or_else(|| exported.clone());
        if exported.definition.path() != &row.path
            || owner.definition.path() != &row.path
            || private.insert(row.path.clone(), owner).is_some()
        {
            return Err(failure("declaration membership"));
        }
        public.push(exported);
    }
    let mut validations = hirsel_validate::ValidatedSignatures::default();
    let base = Arc::new(
        hirsel_validate::ViewTable::new(&types, public, None, &mut validations, None)
            .map_err(|e| failure(format!("declarations: {e:?}")))?,
    );
    profile(started, "declarations checked");
    let bodies = product
        .rows
        .iter()
        .filter_map(|row| row.body.map(|reference| (row.path.clone(), reference)))
        .collect();
    let mut constants = BTreeMap::new();
    for row in &product.rows {
        if let Some(reference) = row.constant {
            if reference.domain != CONSTANT {
                return Err(failure("constant domain"));
            }
            let value: hirsel_tir::Materialized = decode(
                &records,
                get(store, reference)?,
                &anchors,
                &*reserve,
                &mut owners,
            )?;
            constants.insert(row.path.clone(), Arc::new(value));
        }
    }
    owners.extend(header_owners);
    if product.constants.iter().collect::<BTreeSet<_>>().len() != product.constants.len()
        || product
            .constants
            .iter()
            .any(|path| !constants.contains_key(path))
    {
        return Err(failure("constant initialization order"));
    }
    let mut instances = hirsel_tir::InstanceBodies::default();
    for (path, values) in product.instances {
        let id = DefId::intern(&path);
        if !instances.declarations.insert(id) {
            return Err(failure("duplicate instance declaration"));
        }
        for (arguments, definition) in values {
            let key = hirsel_tir::InstanceBodies::key(&types, &arguments)
                .ok_or_else(|| failure("open instance"))?;
            let definition = DefId::intern(&definition);
            if instances
                .instances
                .entry(id)
                .or_default()
                .insert(
                    key,
                    hirsel_tir::InstanceBody {
                        arguments,
                        definition,
                    },
                )
                .is_some()
            {
                return Err(failure("duplicate instance"));
            }
        }
    }
    profile(started, "body descriptors loaded");
    let revision = ProgramRevision::encode(
        &product.entry.to_string(),
        product
            .nodes
            .into_iter()
            .map(
                |(identity, content, dependencies)| hirsel_run_schema::ProgramNode {
                    identity,
                    content,
                    dependencies,
                },
            ),
    )
    .map_err(|e| failure(e.to_string()))?;
    let checked_runtime = Arc::new(crate::snapshot::checked_runtime_from_declarations(
        private.values().cloned(),
        Some(&DefPath::core(hirsel_interp::library::FORGET_OPERATION)),
    ));
    let signature = private
        .get(&product.entry)
        .cloned()
        .ok_or_else(|| failure("missing entry declaration"))?;
    let products = Arc::new(Products {
        types: types.clone(),
        bodies,
        imports: Mutex::new(BodyImports {
            definitions: BTreeMap::new(),
            owners: Vec::new(),
            environments: BTreeMap::new(),
            validations,
        }),
        records,
        anchors,
        public: base,
        private,
        store: store.clone(),
        inputs: current.clone(),
        limit,
        retain: reserve,
        constants,
        order: product.constants,
        instances,
        _admission: Arc::new(owners),
    });
    let runtime = Arc::new(crate::queries::runtime_types_over(
        types,
        hirsel_interp::aggregate::Serialization::demanded(
            products.types.clone(),
            Arc::new(RuntimeRows {
                builtins: product
                    .rows
                    .iter()
                    .filter(|row| row.builtin)
                    .map(|row| row.path.clone())
                    .collect(),
                products: products.clone(),
            }),
        ),
    ));
    let definition = products.definition(&product.entry)?;
    profile(started, "runtime rebuilt");
    Ok(AcceptedExecutionProduct {
        entry: product.entry,
        definition,
        signature,
        sources,
        runtime,
        checked_runtime,
        code: code(product.code),
        callees: product
            .callees
            .into_iter()
            .map(|(path, value)| (path, facts(value)))
            .collect(),
        entries: product
            .entries
            .into_iter()
            .map(|(path, (value, call))| {
                (
                    path,
                    (
                        facts(value),
                        if call {
                            CallForm::Call
                        } else {
                            CallForm::Compute
                        },
                    ),
                )
            })
            .collect(),
        revision,
        foreign_unreplayable: product.foreign_unreplayable,
        foreign: ForeignUse {
            externs: product.externs,
            unmodeled: product.unmodeled,
        },
        products,
    })
}

/// Lookup never compiles. A caller independently observes the complete current
/// loader inputs and import identity before it can use this durable product.
#[allow(clippy::too_many_arguments)]
pub fn import_execution_product(
    revision: Option<[u8; 32]>,
    entry: &DefPath,
    observations: ContentHash,
    imports: ContentHash,
    compiler: ContentHash,
    policy: ContentHash,
    options: ContentHash,
    allowance: hirsel_engine_admission::MemoryReservation,
) -> Result<Option<Arc<AcceptedExecutionProduct>>, QueryError> {
    let store = durable_store()?;
    let inputs = input_index(entry, observations, imports, compiler, policy, options);
    let association = match revision {
        Some(revision) => store
            .lookup(revision_input_index(&revision, inputs))
            .or_else(|| store.lookup(index(&revision, entry))),
        None => store.lookup(inputs),
    };
    let Some(association) = association else {
        crate::queries::operation_owner_trace(format_args!(
            "accepted-execution lookup=miss entry={entry}"
        ));
        return if revision.is_some() {
            Err(failure("recording's accepted product is unavailable"))
        } else {
            Ok(None)
        };
    };
    if association.reference.domain != LOCATOR {
        return Err(failure("locator domain"));
    }
    let pool = allowance.budget().clone();
    drop(allowance);
    let requirement = hirsel_product_codec::DecodeRequirement::<Locator>::plain(get(
        &store,
        association.reference,
    )?)
    .ok_or_else(|| failure("invalid locator"))?;
    let _locator_owner = pool
        .reserve(requirement.required_bytes())
        .map_err(|_| failure("locator exceeds admission"))?;
    let product = requirement
        .decode_reserved(|_| Some(()), |_, _| None)
        .ok_or_else(|| failure("locator exceeds admission"))?;
    // Authorize the exact root before decoding any portable type reference.
    let current = product.inputs();
    if hirsel_validate::VerifierCache::directory_store(store.clone())
        .execution_product(&current, product.root)
        .is_none()
    {
        return Err(failure("missing trusted source binding"));
    }
    if product.observations != observations.0 {
        return Ok(None);
    }
    if product.imports != imports.0
        || product.compiler != compiler.0
        || product.policy != policy.0
        || product.options != options.0
        || &product.entry != entry
    {
        return Err(failure(format!(
            "stale execution identity: imports={} compiler={} policy={} options={} entry={}",
            product.imports != imports.0,
            product.compiler != compiler.0,
            product.policy != policy.0,
            product.options != options.0,
            &product.entry != entry
        )));
    }
    let root = product.root;
    drop(product);
    let limit = pool.free();
    let execution = load(&store, root, &current, limit, move |bytes| {
        pool.reserve(bytes)
            .ok()
            .map(|reservation| Arc::new(reservation) as Arc<dyn Send + Sync>)
            .ok_or_else(|| failure("import exceeds admission"))
    })?;
    if revision.is_some_and(|revision| execution.revision.digest() != revision) {
        return Err(failure("selected semantic revision differs"));
    }
    crate::queries::operation_owner_trace(format_args!(
        "accepted-execution lookup=hit entry={entry}"
    ));
    Ok(Some(Arc::new(execution)))
}

/// Semantic options shared by source publication and replay observation.
pub fn execution_options(
    config: &crate::CompileConfig,
    limits: crate::ComptimeLimits,
) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.execution.options.v1",
        [
            &[u8::from(config.verify == crate::Verify::On)][..],
            &limits.budget.to_le_bytes(),
            &(limits.call_depth as u64).to_le_bytes(),
            &hirsel_tir::CHARGING_VERSION.to_le_bytes(),
            &[match config.lowering {
                hirsel_query_products::LoweringPolicy::Comptime => 0,
                hirsel_query_products::LoweringPolicy::Reachable => 1,
            }],
            std::env::consts::ARCH.as_bytes(),
            std::env::consts::OS.as_bytes(),
            &usize::BITS.to_le_bytes(),
            &[u8::from(cfg!(target_endian = "little"))],
        ],
    )
}
impl QueryEngine {
    /// Loader discovery has selected all imported implementations; no source
    /// header, body or private inference query is needed to read their identity.
    pub fn execution_import_context(&mut self) -> (ContentHash, ContentHash) {
        // Source indexing publishes this manifest revision before snapshots
        // derive their policy. Discovery has independently read the same
        // namespace; reproduce that policy state without parsing declarations.
        self.published_manifest = Some(self.declarations.inputs().manifest_revision);
        (
            self.catalog.installation_identity(),
            self.source_verification_policy(),
        )
    }
}
