//! The S0 signature inventory needed by checked-product encoding. Fingerprint
//! coverage is captured explicitly; an inherited body read log is not coverage.
use super::*;

use hirsel_proof_export::SignatureSection;

struct Input {
    public: hirsel_types::SignatureRef,
    owner: Option<hirsel_types::SignatureRef>,
    coverage: Vec<SignatureDependency>,
}
/// Immutable header inputs bound to the same ordinary front producer as Check.
/// Shared signature payloads remain owned by that producer's snapshot grant.
/// This contains neither checked bodies nor permission to demand one.
pub struct FrontCheckedSignatures {
    inputs: BTreeMap<DefPath, Input>,
    front: Arc<FrontBodyQueries>,
    environment: hirsel_proof_export::Environment,
    registrations: Vec<Vec<u8>>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
impl FrontCheckedSignatures {
    /// Canonical records of the accepted S0 namespace bound to this producer.
    /// Later registration transactions require a new producer and inventory.
    pub fn registrations(&self) -> &[Vec<u8>] {
        &self.registrations
    }
    pub fn environment(&self) -> &hirsel_proof_export::Environment {
        &self.environment
    }
    pub fn signatures(&self) -> impl Iterator<Item = (&DefPath, &hirsel_types::SignatureRef)> {
        self.inputs
            .iter()
            .map(|(path, input)| (path, &input.public))
    }
    pub fn owner_view(&self, path: &DefPath) -> Option<&hirsel_types::SignatureRef> {
        self.inputs.get(path)?.owner.as_ref()
    }
    pub fn coverage(&self, path: &DefPath) -> Option<&[SignatureDependency]> {
        self.inputs.get(path).map(|input| input.coverage.as_slice())
    }
    /// Match the exact source producer, rather than only its signature epoch:
    /// a body-only edit can legitimately reuse the old signature snapshot.
    pub fn matches(&self, front: &Arc<FrontBodyQueries>) -> bool {
        Arc::ptr_eq(&self.front, front)
    }
    pub(crate) fn signature(&self, path: &DefPath) -> Option<&hirsel_types::SignatureRef> {
        self.inputs.get(path).map(|input| &input.public)
    }
    /// Physical payload bound for these already captured public/private
    /// signature values, independently of input table and source owners.
    /// Shared backing is conservatively counted per occurrence; no query,
    /// type loading or allocation occurs while inspecting the payloads.
    pub fn signature_payload_bytes(&self) -> Option<u64> {
        self.inputs.values().try_fold(0u64, |bytes, input| {
            let bytes = bytes.checked_add(
                crate::memo::retained::retained_signature_payload_bytes(&input.public),
            )?;
            match &input.owner {
                Some(owner) => bytes.checked_add(
                    crate::memo::retained::retained_signature_payload_bytes(owner),
                ),
                None => Some(bytes),
            }
        })
    }
    pub(crate) fn sections_clone_bytes(&self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.tree::<DefPath, SignatureSection>(self.inputs.len());
        let mut payload_bytes = 0u64;
        for (path, input) in &self.inputs {
            weight.path(path);
            weight.path(path);
            weight.flat_vec(&input.coverage);
            let mut payloads = crate::memo::retained::Weight::default();
            payloads.signature(&input.public);
            if let Some(owner) = &input.owner {
                payloads.signature(owner);
            }
            payload_bytes = payload_bytes.checked_add(payloads.bytes)?;
        }
        weight.bytes().checked_add(payload_bytes)
    }
    /// Format one already captured section in the caller's admitted scratch.
    /// No signature query, source read or compiler phase runs here.
    pub fn section(&self, path: &DefPath) -> Option<SignatureSection> {
        let input = self.inputs.get(path)?;
        Some(SignatureSection {
            path: path.clone(),
            dependencies: input.coverage.clone(),
            public: (*input.public).clone(),
            owner_view: input.owner.as_ref().map(|owner| (**owner).clone()),
        })
    }
}
impl QueryEngine {
    fn retains_signature_inputs(&self, snapshot: &Arc<SignatureSnapshot>) -> bool {
        self.snapshot.as_ref().is_some_and(|(stamp, current)| {
            *stamp == self.snapshot_stamp()
                && current
                    .get()
                    .is_some_and(|current| Arc::ptr_eq(snapshot, &current))
        })
    }
    fn validate_checked_registrations(
        &mut self,
        front: &FrontBodyQueries,
    ) -> Result<(), QueryError> {
        if !self.staged_constants.is_empty() || !self.staged_values.is_empty() {
            return Err(QueryError::NotImplemented);
        }
        let paths: Vec<_> = self.generated.keys().cloned().collect();
        for path in paths {
            let accepted = front.registration(&path).ok_or_else(|| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "checked signature inputs".into(),
                    message: format!(
                        "generated declaration lacks its accepted front registration: {path}"
                    ),
                })
            })?;
            accepted.validate(self)?;
            if self
                .generated
                .get(&path)
                .is_none_or(|source| source.text != accepted.text())
            {
                return Err(QueryError::NotImplemented);
            }
        }
        Ok(())
    }
    /// Header-capture hold including construction metadata, without body
    /// checking, Lower, body decode or constructing the retained input table.
    pub fn front_checked_signatures_bytes(
        &mut self,
        front: &Arc<FrontBodyQueries>,
    ) -> Result<u64, QueryError> {
        let snapshot = front.signatures();
        if !self.retains_signature_inputs(&snapshot) {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "checked signature inputs".into(),
                message: "captured header snapshot differs from current source inputs".into(),
            }));
        }
        self.validate_checked_registrations(front)?;
        let capture = self.capture_checked_signature_inputs(front.clone())?;
        let paths: BTreeSet<_> = self
            .source_paths()
            .into_iter()
            .chain(self.builtins.keys().cloned())
            .chain(
                self.registry
                    .signatures()
                    .map(|s| s.definition.path().clone()),
            )
            .collect();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<FrontCheckedSignatures>() + 2 * size_of::<usize>());
        weight.tree::<DefPath, Input>(paths.len());
        weight.add(self.environment.compiler_version.capacity());
        let registrations = self.checked_registration_records();
        weight.vec(&registrations, |w, row| w.add(row.capacity()));
        let mut owned = crate::memo::retained::Weight::default();
        for path in paths {
            let id = DefId::intern(&path);
            if !snapshot.signature(id).is_some_and(|s| s.is_ok()) {
                let Some(signature) = self.builtins.get(&path) else {
                    continue;
                };
                owned.signature(signature);
            }
            weight.path(&path);
            weight.flat_vec(&capture.coverage(&snapshot, &path)?);
        }
        if !self.retains_signature_inputs(&snapshot) {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "checked signature inputs".into(),
                message: "captured header snapshot differs from current source inputs".into(),
            }));
        }
        weight
            .bytes()
            .checked_add(owned.bytes)
            .ok_or(QueryError::NotImplemented)
    }
    /// Capture the exact header and private-view inputs while S0 still owns the
    /// loader. Failed signatures stay absent, as in ordinary checked_products;
    /// the completed Check scope separately retains their failure reads.
    pub fn front_checked_signatures(
        &mut self,
        front: Arc<FrontBodyQueries>,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<FrontCheckedSignatures>, QueryError> {
        let invalid = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "checked signature inputs".into(),
                message: "signature inventory exceeds admission or differs from the selected front"
                    .into(),
            })
        };
        if self.front_checked_signatures_bytes(&front)? > allowance.bytes() {
            return Err(invalid());
        }
        let snapshot = front.signatures();
        if !self.retains_signature_inputs(&snapshot) {
            return Err(invalid());
        }
        let inputs = self.capture_checked_signature_inputs(front)?;
        let result = inputs.materialize(allowance)?;
        if !self.retains_signature_inputs(&snapshot) {
            return Err(invalid());
        }
        Ok(result)
    }
    fn capture_checked_signature_inputs(
        &mut self,
        front: Arc<FrontBodyQueries>,
    ) -> Result<CapturedCheckedSignatures, QueryError> {
        if !self.retains_signature_inputs(&front.signatures()) {
            return Err(header_refusal());
        }
        self.validate_checked_registrations(&front)?;
        Ok(CapturedCheckedSignatures {
            paths: self
                .source_paths()
                .into_iter()
                .chain(self.builtins.keys().cloned())
                .chain(
                    self.registry
                        .signatures()
                        .map(|s| s.definition.path().clone()),
                )
                .collect(),
            front,
            builtins: self.builtins.clone(),
            registry: self.registry.clone(),
            environment: self.proof_environment(),
            registrations: self.checked_registration_records(),
        })
    }
    /// Size the immutable export factory without demanding any header product.
    pub fn front_proof_signatures_bytes(
        &mut self,
        front: &Arc<FrontBodyQueries>,
    ) -> Result<u64, QueryError> {
        Ok(self
            .capture_checked_signature_inputs(front.clone())?
            .retained_bytes())
    }
    pub(super) fn deferred_proof_signatures(
        &mut self,
        front: Arc<FrontBodyQueries>,
        mut allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<ProofHeaders, QueryError> {
        let inputs = self.capture_checked_signature_inputs(front)?;
        let factory = allowance
            .split(inputs.retained_bytes())
            .ok_or_else(header_refusal)?;
        Ok(ProofHeaders::Deferred {
            inputs,
            allowance: std::sync::Mutex::new(Some(allowance)),
            result: std::sync::OnceLock::new(),
            _factory: factory,
        })
    }
}
fn header_refusal() -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "checked signature inputs".into(),
        message: "signature inventory exceeds admission or differs from the selected front".into(),
    })
}
/// Captured immutable input to the existing checked-signature producer. The
/// ordinary Check graph retains this factory; export alone demands its table.
pub(super) struct CapturedCheckedSignatures {
    paths: BTreeSet<DefPath>,
    front: Arc<FrontBodyQueries>,
    builtins: BTreeMap<DefPath, hirsel_types::SignatureRef>,
    registry: hirsel_registry::BuiltinRegistry,
    environment: hirsel_proof_export::Environment,
    registrations: Vec<Vec<u8>>,
}
impl CapturedCheckedSignatures {
    fn retained_bytes(&self) -> u64 {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<ProofHeaders>());
        weight.tree::<DefPath, ()>(self.paths.len());
        for path in &self.paths {
            weight.path(path);
        }
        weight.add(self.environment.compiler_version.capacity());
        weight.vec(&self.registrations, |w, row| w.add(row.capacity()));
        weight.tree::<DefPath, hirsel_types::SignatureRef>(self.builtins.len());
        let mut owned = crate::memo::retained::Weight::default();
        for (path, signature) in &self.builtins {
            weight.path(path);
            owned.signature(signature);
        }
        weight.bytes().saturating_add(owned.bytes)
    }
    fn coverage(
        &self,
        snapshot: &SignatureSnapshot,
        path: &DefPath,
    ) -> Result<Vec<SignatureDependency>, QueryError> {
        if let Some(product) = snapshot
            .header_products()
            .and_then(|headers| headers.product(DefId::intern(path)))
        {
            let product = product?;
            let output = product.result.as_ref().map_err(Clone::clone)?;
            return Ok(product.entry.as_ref().map_or_else(
                || output.dependencies.clone(),
                |entry| entry.covered.clone(),
            ));
        }
        if self.builtins.contains_key(path) || self.registry.get(path).is_some() {
            return Ok(Vec::new());
        }
        Err(QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "checked signature inputs".into(),
            message: format!("missing captured signature coverage: {path}"),
        }))
    }
    fn materialize(
        &self,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<FrontCheckedSignatures>, QueryError> {
        let snapshot = self.front.signatures();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<FrontCheckedSignatures>() + 2 * size_of::<usize>());
        weight.tree::<DefPath, Input>(self.paths.len());
        weight.add(self.environment.compiler_version.capacity());
        weight.vec(&self.registrations, |w, row| w.add(row.capacity()));
        if weight.bytes() > allowance.bytes() {
            return Err(header_refusal());
        }
        let mut inputs = BTreeMap::new();
        let mut owned_signatures = crate::memo::retained::Weight::default();
        for path in &self.paths {
            let id = DefId::intern(path);
            let public = match snapshot.signature(id) {
                Some(Ok(signature)) => signature,
                _ => match self.builtins.get(path) {
                    Some(signature) => {
                        owned_signatures.signature(signature);
                        signature.clone()
                    }
                    None => continue,
                },
            };
            let owner = snapshot
                .body_view(&path.0.package, id)
                .and_then(Result::ok)
                .filter(|owner| **owner != *public);
            let coverage = self.coverage(&snapshot, path)?;
            weight.path(path);
            weight.flat_vec(&coverage);
            if weight
                .bytes()
                .checked_add(owned_signatures.bytes)
                .is_none_or(|bytes| bytes > allowance.bytes())
            {
                return Err(header_refusal());
            }
            inputs.insert(
                path.clone(),
                Input {
                    public,
                    owner,
                    coverage,
                },
            );
        }
        Ok(Arc::new(FrontCheckedSignatures {
            inputs,
            front: self.front.clone(),
            environment: self.environment.clone(),
            registrations: self.registrations.clone(),
            _allowance: allowance,
        }))
    }
}
pub(super) enum ProofHeaders {
    Ready(Arc<FrontCheckedSignatures>),
    Deferred {
        inputs: CapturedCheckedSignatures,
        allowance: std::sync::Mutex<Option<hirsel_engine_admission::MemoryReservation>>,
        result: std::sync::OnceLock<Result<Arc<FrontCheckedSignatures>, QueryError>>,
        _factory: hirsel_engine_admission::MemoryReservation,
    },
}
impl ProofHeaders {
    pub(super) fn matches(&self, front: &Arc<FrontBodyQueries>) -> bool {
        match self {
            Self::Ready(headers) => headers.matches(front),
            Self::Deferred { inputs, .. } => Arc::ptr_eq(&inputs.front, front),
        }
    }
    pub(super) fn get(&self) -> Result<&Arc<FrontCheckedSignatures>, QueryError> {
        match self {
            Self::Ready(headers) => Ok(headers),
            Self::Deferred {
                inputs,
                allowance,
                result,
                ..
            } => result
                .get_or_init(|| {
                    let allowance = allowance
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                        .ok_or_else(header_refusal)?;
                    inputs.materialize(allowance)
                })
                .as_ref()
                .map_err(Clone::clone),
        }
    }
}
