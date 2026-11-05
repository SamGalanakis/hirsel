//! Ordinary graph identity; decoded equations belong to the seal consumer.
use super::*;
use hirsel_rc::seal_demand::equations::{Function, LocalDemandEquations};

#[derive(Clone)]
pub struct StoredSealEquations {
    store: Arc<dyn ResultStore>,
    record: Reference,
    identity: ContentHash,
    owner: Function,
    values: u32,
    has_seal: bool,
    decoded_bytes: u64,
}
impl StoredSealEquations {
    pub(super) fn capture(
        store: Arc<dyn ResultStore>,
        record: Reference,
        mut equations: LocalDemandEquations,
    ) -> Result<Self, String> {
        equations.validate()?;
        equations.compact();
        let identity = ContentHash::of_bytes(
            &serde_json::to_vec(&equations).map_err(|error| error.to_string())?,
        );
        Ok(Self {
            store,
            record,
            identity,
            owner: equations.owner().clone(),
            values: equations.value_count(),
            has_seal: equations.has_seal(),
            decoded_bytes: equations.retained_bytes(),
        })
    }
    pub fn owner(&self) -> &Function {
        &self.owner
    }
    pub fn value_count(&self) -> u32 {
        self.values
    }
    pub fn has_seal(&self) -> bool {
        self.has_seal
    }
    /// The seal plan reserves this decoded layout before retaining equations.
    pub fn retained_bytes(&self) -> u64 {
        self.decoded_bytes
    }
    pub(crate) fn retained_metadata_bytes(&self) -> u64 {
        let mut weight = hirsel_engine::NativeMetadataWeight::default();
        weight.path(&self.owner.0);
        weight.bytes()
    }
    pub fn open(&self) -> Result<LocalDemandEquations, String> {
        #[derive(Deserialize)]
        struct Graph {
            seal_demand: LocalDemandEquations,
        }
        let bytes = self
            .store
            .get_content(self.record)
            .ok_or("missing seal equations")?;
        if self.record.domain != INSTANCE_DISCOVERY_DOMAIN
            || self.record.domain.reference(&bytes) != Some(self.record)
        {
            return Err("seal equation record identity differs".into());
        }
        let mut equations = serde_json::from_slice::<Graph>(&bytes)
            .map_err(|error| error.to_string())?
            .seal_demand;
        equations.validate()?;
        equations.compact();
        let identity = ContentHash::of_bytes(
            &serde_json::to_vec(&equations).map_err(|error| error.to_string())?,
        );
        if identity != self.identity
            || equations.owner() != &self.owner
            || equations.value_count() != self.values
            || equations.has_seal() != self.has_seal
        {
            return Err("seal equations differ from the selected descriptor".into());
        }
        if equations.retained_bytes() > self.decoded_bytes {
            return Err("seal equations exceed the admitted decoded layout".into());
        }
        Ok(equations)
    }
}
impl PartialEq for StoredSealEquations {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.owner == other.owner
            && self.values == other.values
            && self.has_seal == other.has_seal
    }
}
impl Eq for StoredSealEquations {}
impl std::fmt::Debug for StoredSealEquations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSealEquations")
            .field("owner", &self.owner)
            .field("identity", &self.identity)
            .finish()
    }
}
