//! `remote@1`: query entries and their content exchanged as REAPI component
//! closures through the [`Cas`] + [`ActionCache`] seam only, so a network
//! implementation needs no change here. Every entry and every content blob is
//! one action; its ActionResult names the record first and then the closure
//! members the record cites. An ActionResult is never authority: each fetched
//! blob must match its REAPI digest and the framed reference it is named by,
//! entries still pass their checked decoders and read validation, and this
//! provider has no verifier, so first V always runs locally. Only canonical
//! producer bytes leave the process; verifier rows, receipts and other
//! issuer state never reach this seam.
use super::*;
use hirsel_artifact_store::reapi::{
    ActionCache, ActionDigest, ActionOutput, ActionResult, BlobDigest, Cas,
};

/// Closure members remembered so a cited payload needs no second lookup.
/// Past the bound a member is found through its own content action.
const NAMES: usize = 1 << 16;

pub struct RemoteStore {
    cas: Arc<dyn Cas>,
    actions: Arc<dyn ActionCache>,
    names: Mutex<HashMap<ContentHash, ActionOutput>>,
}
impl std::fmt::Debug for RemoteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteStore")
    }
}

/// The action that names `key`'s entry record. The address already binds the
/// environment, definition, kind and policy.
fn entry_action(key: &EntryKey) -> Vec<u8> {
    [b"hirsel.remote.entry.v1\0".as_slice(), key.0.0.as_slice()].concat()
}
/// The action that names one immutable content record.
fn content_action(reference: Reference) -> Vec<u8> {
    [
        b"hirsel.remote.content.v1\0".as_slice(),
        reference.output.0.0.as_slice(),
        &reference.domain.kind.to_le_bytes(),
        &reference.domain.schema.to_le_bytes(),
        &reference.len.to_le_bytes(),
    ]
    .concat()
}

impl RemoteStore {
    pub fn new(cas: Arc<dyn Cas>, actions: Arc<dyn ActionCache>) -> Self {
        Self {
            cas,
            actions,
            names: Mutex::default(),
        }
    }
    fn remember(&self, outputs: &[ActionOutput]) {
        let mut names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        for output in outputs {
            if names.len() < NAMES || names.contains_key(&output.reference.output.0) {
                names.insert(output.reference.output.0, *output);
            }
        }
    }
    /// The closure member for `reference`: remembered, else its content action.
    fn name(&self, reference: Reference) -> Option<ActionOutput> {
        let remembered = self
            .names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&reference.output.0)
            .copied();
        remembered
            .filter(|output| output.reference == reference)
            .or_else(|| {
                let result = self
                    .actions
                    .get(&ActionDigest::of(&content_action(reference)))
                    .ok()??;
                let [output] = result.outputs.as_slice() else {
                    return None;
                };
                (output.reference == reference).then_some(*output)
            })
    }
    /// Foreign bytes are used only when they match both the blob digest and
    /// the framed reference that names them.
    fn read(&self, output: ActionOutput) -> Option<Bytes> {
        let [Ok(Some(bytes))] =
            <[_; 1]>::try_from(self.cas.batch_read(&[output.blob]).ok()?).ok()?
        else {
            return None;
        };
        (BlobDigest::of(&bytes) == output.blob
            && output.reference.domain.reference(&bytes) == Some(output.reference))
        .then_some(bytes)
    }
    /// Upload the action and the new members, then name the closure. The
    /// ActionCache is updated only after every blob is stored.
    fn publish(&self, action: &[u8], outputs: &[ActionOutput], uploads: Vec<Bytes>) -> bool {
        let digest = ActionDigest::of(action);
        let mut blobs = vec![(digest.0, Bytes::from(action))];
        blobs.extend(
            outputs
                .iter()
                .zip(uploads)
                .map(|(output, bytes)| (output.blob, bytes)),
        );
        let uploaded = self
            .cas
            .batch_update(&blobs)
            .is_ok_and(|statuses| statuses.iter().all(Result::is_ok));
        uploaded
            && self
                .actions
                .update(
                    &digest,
                    &ActionResult {
                        outputs: outputs.to_vec(),
                    },
                )
                .is_ok()
    }
    /// Publish `value` at `key` with the closure members it cites. A member
    /// this provider cannot name leaves the record unpublished: a later miss.
    fn put_record(&self, key: &EntryKey, value: &[u8], cites: impl IntoIterator<Item = Reference>) {
        let Some(record) = ActionResult::output(entry_domain(key.2), value) else {
            return;
        };
        let mut outputs = vec![record];
        for reference in cites {
            let Some(member) = self.name(reference) else {
                return;
            };
            outputs.push(member);
        }
        self.publish(&entry_action(key), &outputs, vec![value.into()]);
    }
}

impl ResultStore for RemoteStore {
    fn capability(&self) -> ProviderCapability {
        ProviderCapability::Writable
    }

    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        let result = self
            .actions
            .get(&ActionDigest::of(&entry_action(key)))
            .ok()??;
        let (&record, members) = result.outputs.split_first()?;
        if record.reference.domain != entry_domain(key.2) {
            return None;
        }
        let bytes = self.read(record)?;
        self.remember(members);
        Some(bytes)
    }
    fn put(&self, key: &EntryKey, value: &[u8]) {
        let cites: Vec<_> = Entry::decode_for(key, &value.into())
            .map(|entry| entry.variants.iter().map(|v| v.output).collect())
            .unwrap_or_default();
        self.put_record(key, value, cites);
    }
    fn associate_product(&self, address: Address, action: Action, reference: Reference) {
        let bytes = serde_json::to_vec(&(address.0, action.0, reference))
            .expect("fixed product association serializes");
        if bytes.len() <= 1024 {
            self.put_record(&product_association_key(address), &bytes, [reference]);
        }
    }
    fn associate_written_product(&self, address: Address, action: Action, reference: Reference) {
        self.associate_product(address, action, reference);
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        self.read(self.name(reference)?)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let output = ActionResult::output(domain, bytes)?;
        if !self.publish(
            &content_action(output.reference),
            &[output],
            vec![bytes.into()],
        ) {
            return None;
        }
        self.remember(&[output]);
        Some(output.reference)
    }
    fn name(&self) -> &'static str {
        "remote@1"
    }
}
