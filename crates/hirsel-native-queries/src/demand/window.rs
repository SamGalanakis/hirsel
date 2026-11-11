//! Admission follows the last reader, independently of ordinary stored rows.
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct State {
    base: u64,
    live: u64,
    peak: u64,
    parts: [u64; 8],
    peak_parts: [u64; 8],
    peak_base: u64,
}
pub(crate) struct WindowBudget {
    owner: Arc<hirsel_engine_admission::MemoryReservation>,
    state: Mutex<State>,
}
pub(crate) struct WindowRead {
    budget: Arc<WindowBudget>,
    bytes: u64,
    kind: usize,
}
impl WindowBudget {
    pub fn retained_bytes() -> u64 {
        (size_of::<Self>() + 2 * size_of::<usize>()) as u64
    }
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
        if total > state.peak {
            state.peak = total;
            state.peak_base = state.base;
            state.peak_parts = state.parts;
        }
        Ok(())
    }
    pub fn read(self: &Arc<Self>, bytes: u64) -> Result<Arc<WindowRead>, String> {
        self.read_for("facts", bytes)
    }
    pub fn read_for(
        self: &Arc<Self>,
        kind: &'static str,
        bytes: u64,
    ) -> Result<Arc<WindowRead>, String> {
        let kind = match kind {
            "facts" => 0,
            "callable-index" => 1,
            "operations" => 2,
            "scc" => 3,
            "published-index" => 4,
            "native-plan" => 5,
            "template" => 6,
            _ => 7,
        };
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
        state.parts[kind] += bytes;
        if total > state.peak {
            state.peak = total;
            state.peak_base = state.base;
            state.peak_parts = state.parts;
        }
        Ok(Arc::new(WindowRead {
            budget: self.clone(),
            bytes,
            kind,
        }))
    }
    pub fn report(&self, label: &str) {
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!(
                "NATIVE_WINDOW_ATTRIBUTION {label} peak={} quota={} base={} facts={} index={} operations={} scc={} published={} native-plan={} template={} other={}",
                s.peak,
                self.owner.bytes(),
                s.peak_base,
                s.peak_parts[0],
                s.peak_parts[1],
                s.peak_parts[2],
                s.peak_parts[3],
                s.peak_parts[4],
                s.peak_parts[5],
                s.peak_parts[6],
                s.peak_parts[7]
            );
        }
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
        state.parts[self.kind] = state.parts[self.kind] - self.bytes + bytes;
        if total > state.peak {
            state.peak = total;
            state.peak_base = state.base;
            state.peak_parts = state.parts;
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for WindowRead {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        state.parts[self.kind] -= self.bytes;
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
        let adapters = self.adapters(instances)?;
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
    /// Only the pretransfer adapter facts, the same content `read` decodes into
    /// `facts.source.adapters`; an operation scope needs nothing else.
    pub fn adapters(
        &self,
        instances: &DemandInstances,
    ) -> Result<Vec<hirsel_lir::ir::Symbol>, String> {
        let bytes = instances
            .store
            .read_product_content(self.adapters)
            .map_err(|error| format!("native adapter facts: {error}"))?;
        if ADAPTERS.reference(&bytes) != Some(self.adapters) {
            return Err("native adapter facts identity differs".into());
        }
        hirsel_codec::from_text(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
            .map_err(|e| format!("native adapter facts: {e:?}"))
    }
    pub fn retained_bytes(&self) -> u64 {
        size_of::<Self>() as u64
            + size_of::<InstanceProduct>() as u64
            + 2 * size_of::<usize>() as u64
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct StoredRecord {
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
    pub(crate) fn record(&self) -> Result<StoredRecord, String> {
        Ok(StoredRecord {
            address: self.product.product.address.0,
            action: self.product.product.action.0,
            archive: self
                .product
                .product
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
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
    pub(crate) fn from_record(store: &dyn ResultStore, row: StoredRecord) -> Result<Self, String> {
        let archive = store
            .copy_product_archive(row.archive)
            .map_err(|error| format!("window product archive: {error}"))?;
        Ok(Self {
            product: Arc::new(InstanceProduct {
                product: hirsel_engine_runtime::ProductRef::import_archive(
                    hirsel_engine_store::Address(row.address),
                    hirsel_engine_store::Action(row.action),
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
