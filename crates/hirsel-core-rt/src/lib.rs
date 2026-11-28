//! FORM v1 memory operations. Emitters own layout, moves and drop recipes.
//! Logical capacity refusal is recoverable; host exhaustion is a run stop.

#[cfg(test)]
mod embed_stop_tests;
#[cfg(test)]
#[path = "../../../tools/test_support/loopback.rs"]
#[allow(dead_code)]
mod loopback;
#[cfg(test)]
hirsel_guard::install!();

pub mod abi;
pub mod abi_host;
pub mod abi_layout;
pub mod abi_tasks;
#[cfg(feature = "allocation-count")]
mod allocation_count;
pub mod allocator;
mod argon2id;
mod backing_observation;
pub mod block;
pub mod causes;
pub mod channel;
pub mod code;
pub mod contain;
pub mod counted;
mod coverage;
mod crc;
mod crc32c;
pub mod cred;
mod data_decode;
mod destination;
#[cfg(not(feature = "standalone"))]
pub mod embed;
pub mod entry;
mod env;
#[path = "host/exec_grant.rs"]
mod exec;
#[path = "host/exec_runtime.rs"]
mod exec_runtime;
pub mod fiber;
pub mod flow;
pub mod foreign;
mod growth;
pub mod host;
pub mod host_file;
pub mod host_ops;
pub mod isa;
pub mod isolate;
pub mod latest;
#[cfg(feature = "record")]
mod law;
pub mod limit;
mod media;
#[cfg(feature = "prof")]
mod mem_prof;
#[cfg(any(test, feature = "prof"))]
mod mem_sampler;
pub mod net;
pub mod os;
mod os_net;
mod os_resolve;
mod os_tls;
use host::unix_api as os_unix;
pub use os_tls::packet_cipher::PacketCipher;
pub mod glue;
#[cfg(any(test, feature = "charge-observation"))]
pub mod peak;
pub mod process;
#[cfg(feature = "prof")]
mod prof_stack;
mod provide;
pub mod provider;
pub mod region;
mod resolve;
mod retention;
pub mod run_stop;
#[cfg(any(test, feature = "testing"))]
pub mod schedule_script;
mod scram;
pub mod secret;
mod sha1;
mod sha256;
mod sha512;
pub mod shared;
mod signature;
pub mod stack;
#[cfg(all(feature = "standalone", not(test)))]
mod standalone_allocator;
mod stdin;
mod stdin_secret;
pub mod steps;
mod stop_signals;
#[cfg(feature = "record")]
pub mod tape;
mod task_heap;
pub mod tasks;
mod term_restore;
#[cfg(feature = "testing")]
pub mod test_provider;
pub mod test_teardown;
pub mod tls;
pub mod tls_server;
mod udp_net;
mod units;
pub mod units_lang;
mod unix_net;
mod zero;
/// The unit connection path's round-trip measurement (`examples/units_conn.rs`).
#[cfg(not(test))]
#[doc(hidden)]
pub use units::ledger::bench::conn_pingpong as units_conn_pingpong;
pub use units_lang::bench::pingpong as units_lang_pingpong;
mod xxh3;

mod frame_list;
mod split_bytes;

use limit::Funding;
use std::sync::atomic::AtomicBool;

/// MEM-22. Also used for the error portion of C allocation results.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    pub requested: u64,
    pub available: u64,
}

/// The hidden first argument of native functions. A task owns its context;
/// custody can carry funding records beyond this context's lifetime.
#[repr(C, align(64))]
pub struct Context {
    // Funding's first word is the one frozen current-record pointer.
    pub(crate) current: Funding,
    /// Frozen event gate. Zero outside a running scheduler; otherwise this
    /// is the pinned Task pointer used by context-first Arc completion hooks.
    /// The root enables it for poll fairness before execution; children inherit it
    /// and install their own Task pointer before publication. It is immutable
    /// for the task's run, then cleared when the root returns.
    pub events: usize,
    /// MEM-30 fast-path window, at the frozen offset CONTEXT_STEPS.
    pub steps: i64,
    /// FAIL-17: the lowest stack address an activation of user code may enter
    /// with, at the frozen offset CONTEXT_STACK_LIMIT. Zero refuses nothing (a
    /// context that does not run on a task stack). A task stack sets it before
    /// the task first runs ([`stack::commit`]); an isolate saves it and a `Stack`
    /// refusal releases the cleanup margin below it ([`stack::hirsel_core_stack_out`]).
    pub stack_limit: usize,
    pub(crate) map_fast_salt: [u64; 2],
    /// The task heap's size-class free-list heads, at the frozen offset
    /// CONTEXT_HEAP_HEADS. Null until generated code pops small blocks inline
    /// (packing slice P5); nothing reads it before then.
    pub(crate) heap_heads: *const u8,
    /// The Place execution this task runs for, at the frozen offset
    /// CONTEXT_EXECUTION (PACK-13): the record of the run the creating thread
    /// served, which outlives every task of that run. Null when the context
    /// was made outside any run, as a direct runtime test makes one.
    pub(crate) execution: *const run_stop::Execution,
    pub(crate) map_hash_key: hirsel_core_layout::map_hash::PreparedKey,
    pub(crate) map_strong_key: hirsel_core_layout::map_hash::PreparedKey,
    /// PACK-9.7: data owner for the current release's sites (`*const CauseData`,
    /// null for host/immortal sites), at the frozen offset CONTEXT_SITE_OWNER.
    pub site_owner: *const causes::CauseData,
    // Contained allocations use the entry that carries their exact logical charge.
    allocation_custody: u8,
    pub(crate) run: *const crate::process::Config,
    /// The limit `stack_limit` returns to when the cleanup margin is restored.
    pub(crate) stack_floor: usize,
    pub(crate) cancel_requested: AtomicBool,
    // Standalone roots and concurrent test tasks close their own heaps.
    owns_heap: bool,
    pub(crate) cancel_shield: usize,
    pub(crate) finalizing: usize,
    pub(crate) bound_depth: usize,
    pub(crate) primary: Option<causes::OwnedCell>,
    pub(crate) causes: causes::Causes,
    pub(crate) task_record: Option<block::Block>,
    pub(crate) active_resource: *mut causes::Resource,
    pub(crate) host_files: *mut host_file::Files,
    pub(crate) isolates: Vec<crate::isolate::Reservation>,
    pub(crate) step_bounds: steps::Bounds,
    /// This task's unspent step account ids ([`steps::Ids`]).
    pub(crate) step_ids: steps::Ids,
    #[cfg(feature = "testing")]
    pub(crate) step_observation: Option<steps::Observation>,
    pub(crate) step_slice_tail: u64,
    pub(crate) regions: region::RegionCache,
    pub(crate) heap: std::cell::OnceCell<std::sync::Arc<task_heap::Heap>>,
    pub(crate) frame_lists: frame_list::FrameLists,
    pub(crate) spare_isolate_cell: Option<causes::ReservedStorage>,
    pub(crate) foreign: *const foreign::Run,
    /// The task's contained instances (FFI-13), dropped at its end.
    pub(crate) contained: foreign::contained::Instances,
    pub(crate) root_limit: u64,
    /// The cause in `causes` that the root limit refused, if any (PROG-3).
    pub(crate) root_refusal: *const causes::Cause,
    /// FAIL-17's glue worklist ([`glue`]).
    pub(crate) glue: Vec<[u64; 4]>,
    decode: Option<data_decode::Pending>,
}

const _: () = {
    assert!(std::mem::offset_of!(Context, map_hash_key) == abi_layout::CONTEXT_MAP_HASH_KEY);
    assert!(std::mem::offset_of!(Context, steps) == abi_layout::CONTEXT_STEPS);
    assert!(std::mem::offset_of!(Context, stack_limit) == abi_layout::CONTEXT_STACK_LIMIT);
};

type MapKeys = (
    hirsel_core_layout::map_hash::PreparedKey,
    [u64; 2],
    hirsel_core_layout::map_hash::PreparedKey,
);

fn map_keys(key: &[u8; 16]) -> MapKeys {
    use hirsel_core_layout::{map_hash::PreparedKey, map_index::derive};

    let (s0, s1, strong) = derive(key);
    (PreparedKey::new(key), [s0, s1], PreparedKey::new(&strong))
}

const _: () = {
    assert!(std::mem::offset_of!(Context, map_fast_salt) == abi_layout::CONTEXT_MAP_FAST_SALT);
    assert!(std::mem::offset_of!(Context, heap_heads) == abi_layout::CONTEXT_HEAP_HEADS);
    assert!(std::mem::offset_of!(Context, execution) == abi_layout::CONTEXT_EXECUTION);
    assert!(std::mem::align_of::<Context>() == abi_layout::CONTEXT_LINE);
    assert!(std::mem::offset_of!(Context, map_strong_key) == abi_layout::CONTEXT_MAP_STRONG_KEY);
    assert!(std::mem::offset_of!(Context, site_owner) == abi_layout::CONTEXT_SITE_OWNER);
    assert!(
        std::mem::offset_of!(Context, allocation_custody) == abi_layout::CONTEXT_ALLOCATION_CUSTODY
    );
};

impl Context {
    fn map_keys(&self) -> MapKeys {
        (self.map_hash_key, self.map_fast_salt, self.map_strong_key)
    }

    fn set_map_keys(&mut self, keys: MapKeys) {
        (self.map_hash_key, self.map_fast_salt, self.map_strong_key) = keys;
    }
    /// The task provider admits its task record and primary cell separately.
    #[inline]
    pub fn new(bytes: u64) -> Self {
        Self::with_funding(Funding::root(bytes))
    }

    pub(crate) fn with_unit_heap(bytes: u64) -> Self {
        let mut context = Self::new(bytes);
        if let Some(heap) = units::heap_for_context() {
            context.share_heap(heap);
        }
        context
    }

    /// True when this task's own execution has recorded a stop (PACK-13).
    #[inline]
    pub(crate) fn execution_ended(&self) -> bool {
        // SAFETY: a run's record outlives every task context made under it.
        match unsafe { self.execution.as_ref() } {
            Some(execution) => execution.is_ended(),
            None => run_stop::is_ended(),
        }
    }

    pub(crate) fn share_heap(&mut self, heap: std::sync::Arc<task_heap::Heap>) {
        self.heap = std::cell::OnceCell::from(heap);
        self.owns_heap = false;
    }

    pub(crate) fn with_funding(current: Funding) -> Self {
        current.normalize();
        let root_limit = current.capacity();
        static DEFAULT_KEYS: std::sync::OnceLock<MapKeys> = std::sync::OnceLock::new();
        let (map_hash_key, map_fast_salt, map_strong_key) =
            *DEFAULT_KEYS.get_or_init(|| map_keys(&[0; 16]));
        Self {
            current,
            events: 0,
            steps: steps::initial_unbounded_steps(),
            step_bounds: steps::Bounds::default(),
            step_ids: steps::Ids::default(),
            heap_heads: std::ptr::null(),
            execution: run_stop::current_execution().map_or(std::ptr::null(), std::ptr::from_ref),
            #[cfg(feature = "testing")]
            step_observation: None,
            step_slice_tail: steps::SLICE,
            regions: region::RegionCache::default(),
            heap: std::cell::OnceCell::new(),
            run: std::ptr::null(),
            stack_limit: 0,
            stack_floor: 0,
            map_hash_key,
            map_fast_salt,
            map_strong_key,
            site_owner: std::ptr::null(),
            allocation_custody: u8::from(run_stop::is_contained()),
            cancel_requested: AtomicBool::new(false),
            owns_heap: true,
            cancel_shield: 0,
            finalizing: 0,
            bound_depth: 0,
            primary: None,
            causes: causes::Causes::default(),
            task_record: None,
            active_resource: std::ptr::null_mut(),
            host_files: std::ptr::null_mut(),
            isolates: Vec::new(),
            frame_lists: frame_list::FrameLists::default(),
            spare_isolate_cell: None,
            foreign: std::ptr::null(),
            contained: Default::default(),
            root_limit,
            root_refusal: std::ptr::null(),
            glue: Vec::new(),
            decode: None,
        }
    }

    #[cfg(feature = "testing")]
    pub fn reset_test_limit(&mut self, bytes: u64) {
        self.current = Funding::root(bytes);
        self.root_limit = bytes;
        self.current.bind();
    }
    /// Harness-only transfer of an owned descriptor into this run's socket records.
    /// # Safety
    /// The fixture runs inside hirsel_core_run, which pins the descriptor registry.
    #[cfg(feature = "testing")]
    pub unsafe fn adopt_fixture_socket(&mut self, fd: std::os::fd::OwnedFd) -> u64 {
        unsafe {
            (*self.host_files)
                .sockets
                .insert(fd)
                .unwrap_or_else(|| run_stop::raise(run_stop::Stop::from_static(b"HostHandles")))
        }
    }
    /// Sets a fixture's program allowance, excluding its original primary
    /// cell, while preserving the scheduler's payer domain.
    #[cfg(feature = "testing")]
    pub fn set_test_available(&mut self, bytes: u64) {
        if self.task_record.is_some()
            && let Some(primary) = &mut self.primary
        {
            primary.exclude_startup_from_test_limit();
        }
        self.current.set_test_available(bytes);
    }

    #[cfg(feature = "testing")]
    pub fn set_test_map_key(&mut self, key: &[u8; 16]) {
        self.set_map_keys(map_keys(key));
    }

    #[cfg(feature = "testing")]
    pub fn inject_admission(&mut self, n: usize, later: bool) {
        self.current.inject(n, later);
    }
    #[cfg(feature = "testing")]
    pub fn admission_attempts(&self) -> usize {
        self.current.attempts()
    }
    /// Native emitter pointer to the frozen current-limit prefix. Dereferencing
    /// requires the task-exclusive discipline of the C ABI.
    #[inline(always)]
    pub fn limit_record(&self) -> *mut limit::LimitRecord {
        self.current.raw_local()
    }

    #[inline]
    pub fn available(&self) -> u64 {
        // SAFETY: native entry/resume and every limit/grant transition install
        // this context's live canonical record before it can execute.
        unsafe { (*self.limit_record()).available }
    }

    #[inline]
    pub fn limit_enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        self.current.enter(bytes)?;
        self.bound_depth = self.bound_depth.wrapping_add(1);
        Ok(())
    }

    #[inline]
    pub fn limit_exit(&mut self) {
        self.bound_depth = self.bound_depth.wrapping_sub(1);
        #[cfg(feature = "prof")]
        {
            crate::mem_prof::slot(self.limit_record().addr(), self.available());
            crate::mem_prof::scope_exit(0);
        }
        self.current.exit();
    }

    #[inline]
    pub(crate) fn payer(&self) -> limit::Payer {
        limit::Payer::from_current(self.limit_record())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        data_decode::discard(self);
        // Release all context-owned heap blocks and limit charges while both
        // `self.current` and `self.heap` are still live and open.
        self.isolates.clear();
        self.causes = causes::Causes::default();
        self.primary = None;
        self.spare_isolate_cell = None;
        self.task_record = None;
        self.regions = region::RegionCache::default();
        self.contained = Default::default();
        while self.bound_depth != 0 {
            self.limit_exit();
        }
        if let Some(heap) = self.heap.take().filter(|_| self.owns_heap) {
            // SAFETY: context destruction has exclusive custody and runs once.
            unsafe { heap.close() };
        }
    }
}

#[cfg(test)]
mod causes_tests;
#[cfg(test)]
mod compact_tests;
#[cfg(test)]
mod counted_tests;
#[cfg(test)]
mod region_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod exec_slots_tests;
#[cfg(test)]
mod provider_s0_tests;
#[cfg(test)]
mod provider_tests;
#[cfg(test)]
mod unix_join_tests;
#[cfg(test)]
mod unix_race_tests;

#[cfg(test)]
mod tasks_tests;

#[cfg(test)]
mod schedule_tests;

#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod host_file_tests;
#[cfg(test)]
mod host_ops_tests;
#[cfg(test)]
mod net_idle_tests;
#[cfg(test)]
mod net_tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod openat2_required;

#[cfg(test)]
mod test_process;

#[cfg(test)]
mod isolate_tests;

pub mod abi_sites;

#[cfg(test)]
mod sites_tests;

#[cfg(feature = "prof")]
pub(crate) mod prof;

#[cfg(all(feature = "prof", feature = "testing"))]
pub use prof::testing as prof_testing;

#[cfg(feature = "prof")]
#[unsafe(no_mangle)]
pub static hirsel_core_prof_runtime: u8 = 1;
