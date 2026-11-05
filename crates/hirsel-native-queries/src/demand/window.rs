//! Admission follows the last reader, independently of ordinary stored rows.
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct State {
    base: u64,
    live: u64,
    peak: u64,
}
pub(crate) struct WindowBudget {
    owner: Arc<hirsel_engine_admission::MemoryReservation>,
    state: Mutex<State>,
}
pub(crate) struct WindowRead {
    budget: Arc<WindowBudget>,
    bytes: u64,
}
impl WindowBudget {
    pub fn new(owner: Arc<hirsel_engine_admission::MemoryReservation>) -> Arc<Self> {
        Arc::new(Self {
            owner,
            state: Mutex::new(State::default()),
        })
    }
    pub fn set_base(&self, bytes: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let total = bytes
            .checked_add(state.live)
            .ok_or("native window metadata overflow")?;
        if total > self.owner.bytes() {
            return Err(format!(
                "native window base admission refused: {total}/{} B",
                self.owner.bytes()
            ));
        }
        state.base = bytes;
        state.peak = state.peak.max(total);
        Ok(())
    }
    pub fn read(self: &Arc<Self>, bytes: u64) -> Result<Arc<WindowRead>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let live = state
            .live
            .checked_add(bytes)
            .ok_or("native window metadata overflow")?;
        let total = state
            .base
            .checked_add(live)
            .ok_or("native window metadata overflow")?;
        if total > self.owner.bytes() {
            return Err(format!(
                "native window admission refused: base {}, live {}, requested {bytes}, quota {} B",
                state.base,
                state.live,
                self.owner.bytes()
            ));
        }
        state.live = live;
        state.peak = state.peak.max(total);
        Ok(Arc::new(WindowRead {
            budget: self.clone(),
            bytes,
        }))
    }
    pub fn peak(&self) -> (u64, u64) {
        (
            self.state.lock().unwrap_or_else(|e| e.into_inner()).peak,
            self.owner.bytes(),
        )
    }
}
impl WindowRead {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn resize(&mut self, bytes: u64) -> Result<(), String> {
        let mut state = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        let live = state
            .live
            .checked_sub(self.bytes)
            .and_then(|live| live.checked_add(bytes))
            .ok_or("native window metadata overflow")?;
        let total = state
            .base
            .checked_add(live)
            .ok_or("native window metadata overflow")?;
        if total > self.budget.owner.bytes() {
            return Err(format!(
                "native window metadata admission refused: base {}, live {}, requested {bytes}, quota {} B",
                state.base,
                state.live - self.bytes,
                self.budget.owner.bytes()
            ));
        }
        state.live = live;
        state.peak = state.peak.max(total);
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for WindowRead {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        state.live = state
            .live
            .checked_sub(self.bytes)
            .expect("native window reader charge");
    }
}

const ADAPTERS: Domain = Domain {
    kind: INSTANCE_KIND,
    schema: 3,
};
/// Identity and an ordinary recipe for reopening facts. No decoded fact owner
/// crosses a consumer boundary, and this object conveys no native C authority.
#[derive(Clone)]
pub struct StoredInstance {
    pub product: Arc<InstanceProduct>,
    adapters: Reference,
    bytes: u64,
}
impl StoredInstance {
    pub fn capture(store: &dyn ResultStore, output: &DiscoveredInstance) -> Result<Self, String> {
        let bytes = hirsel_codec::to_text(&output.facts.source.adapters).into_bytes();
        let adapters = store
            .put_content(ADAPTERS, &bytes)
            .ok_or("native adapter publication failed")?;
        if ADAPTERS.reference(&bytes) != Some(adapters) {
            return Err("native adapter publication identity differs".into());
        }
        Ok(Self {
            product: output.product.clone(),
            adapters,
            bytes: output.retained_metadata_bytes(),
        })
    }
    pub fn read(
        &self,
        instances: &DemandInstances,
        key: &InstanceAddress,
    ) -> Result<Arc<DiscoveredInstance>, String> {
        let mut lease = instances
            .window
            .as_ref()
            .map(|window| window.read(self.bytes))
            .transpose()
            .map_err(|e| format!("native facts for {}: {e}", key.instance.definition()))?;
        let bytes = instances
            .store
            .get_content(self.adapters)
            .ok_or("missing native adapter facts")?;
        if ADAPTERS.reference(&bytes) != Some(self.adapters) {
            return Err("native adapter facts identity differs".into());
        }
        let adapters =
            hirsel_codec::from_text(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
                .map_err(|e| format!("native adapter facts: {e:?}"))?;
        let mut output = instances.decode_completed(key, self.product.clone(), adapters)?;
        // Decode uses the consumer's scratch. Reopening may change allocation
        // capacities and structural sharing without changing any content.
        // Transfer the actual retained representation into the same quota
        // before a fact or reader can escape this consumer.
        if let Some(lease) = &mut lease {
            Arc::get_mut(lease)
                .ok_or("unpublished native reader is shared")?
                .resize(output.retained_metadata_bytes())
                .map_err(|e| format!("native facts for {}: {e}", key.instance.definition()))?;
        }
        Arc::make_mut(&mut output.facts)._window = lease.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut output.facts).source)._window = lease.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut output.facts).body)._window = lease.clone();
        output._window = lease;
        Ok(Arc::new(output))
    }
    pub fn retained_bytes(&self) -> u64 {
        size_of::<Self>() as u64
            + size_of::<InstanceProduct>() as u64
            + 2 * size_of::<usize>() as u64
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredRecord {
    address: ContentHash,
    action: ContentHash,
    archive: Reference,
    value: Reference,
    layouts: Reference,
    equations: Reference,
    discovery: Reference,
    statics: Reference,
    adapters: Reference,
    bytes: u64,
}
impl StoredInstance {
    pub(super) fn record(&self) -> Result<StoredRecord, String> {
        Ok(StoredRecord {
            address: self.product.product.address.0,
            action: self.product.product.action.0,
            archive: self
                .product
                .product
                .archive(hirsel_engine::runtime::ArchiveAdmission::Retained)
                .map_err(|e| e.to_string())?
                .reference(),
            value: self.product.value,
            layouts: self.product.layout_closure,
            equations: self.product.local_equations,
            discovery: self.product.discovery,
            statics: self.product.statics_finalizers,
            adapters: self.adapters,
            bytes: self.bytes,
        })
    }
    pub(super) fn from_record(store: &dyn ResultStore, row: StoredRecord) -> Result<Self, String> {
        let archive = store
            .copy_product_archive(row.archive)
            .ok_or("missing window product archive")?;
        Ok(Self {
            product: Arc::new(InstanceProduct {
                product: hirsel_engine::runtime::ProductRef::import_archive(
                    hirsel_engine::store::Address(row.address),
                    hirsel_engine::store::Action(row.action),
                    archive,
                ),
                value: row.value,
                layout_closure: row.layouts,
                local_equations: row.equations,
                discovery: row.discovery,
                statics_finalizers: row.statics,
            }),
            adapters: row.adapters,
            bytes: row.bytes,
        })
    }
}
