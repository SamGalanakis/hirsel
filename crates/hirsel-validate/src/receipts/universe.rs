//! Local verifier publication during admitted universe construction.
//! Hash tables use the universe region; immutable Bytes retain their actual
//! backing allocation bound before a row can publish a reference.
use crate::region::{Map, Region};
use crate::{FactRefusal, ValidationMemory, VerifierRows, ViewRefusal};
use hirsel_artifact_store::{Bytes, ContentReadError, Contents, Domain, Reference};
use hirsel_syntax::ContentHash;
use std::sync::{Arc, Mutex};

pub(super) struct UniverseReceipts {
    storage: Region,
    rows: Mutex<Map<ContentHash, Reference>>,
    contents: Mutex<Map<ContentHash, (Reference, Bytes)>>,
    memory: Arc<dyn ValidationMemory>,
    _owner: Arc<dyn Send + Sync>,
}
impl UniverseReceipts {
    pub(super) fn new(memory: Arc<dyn ValidationMemory>) -> Result<Arc<Self>, ViewRefusal> {
        let owner = memory.retain(
            crate::memory::arc::<Self>()
                + crate::memory::arc::<crate::VerifierCache>()
                + crate::memory::arc::<crate::FactGraph>()
                + crate::memory::arc::<()>(),
        )?;
        let storage = Region::default();
        storage.retain(&memory)?;
        Ok(Arc::new(Self {
            rows: Mutex::new(storage.map()),
            contents: Mutex::new(storage.map()),
            storage,
            memory,
            _owner: owner,
        }))
    }
}
impl Contents for UniverseReceipts {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        let contents = self.contents.lock().unwrap();
        let (actual, bytes) = contents
            .get(&reference.output.0)
            .ok_or(ContentReadError::NotFound)?;
        if *actual != reference {
            return Err(ContentReadError::InvalidContent);
        }
        Ok(bytes.clone())
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let reference = domain.reference(bytes)?;
        let mut contents = self.contents.lock().unwrap();
        if let Some((actual, _)) = contents.get(&reference.output.0) {
            return (*actual == reference).then_some(reference);
        }
        let owner = self
            .memory
            .retain(Bytes::owned_with_owner_bytes(bytes.len() as u64)?)
            .ok()?;
        let bytes = Bytes::from(bytes).retain_owner(owner);
        self.storage
            .insert(&mut contents, reference.output.0, (reference, bytes))
            .ok()?;
        Some(reference)
    }
}
impl VerifierRows for UniverseReceipts {
    fn retain_fact(&self, bytes: u64) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
        self.memory.retain(bytes).map(Some).map_err(|_| FactRefusal)
    }
    fn row(&self, key: ContentHash) -> Option<Reference> {
        self.rows.lock().unwrap().get(&key).copied()
    }
    fn set_row(&self, key: ContentHash, reference: Reference) {
        let _ = self
            .storage
            .insert(&mut self.rows.lock().unwrap(), key, reference);
    }
}
