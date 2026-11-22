//! FORM v1 memory operations. Emitters own layout, moves and drop recipes.
//! Logical capacity refusal is recoverable; host exhaustion is a run stop.

#[cfg(test)]
hirsel_guard::install!();

pub mod abi;
pub mod abi_host;
pub mod abi_layout;
pub mod abi_tasks;
#[cfg(feature = "allocation-count")]
mod allocation_count;
mod allocator;
mod backing_observation;
pub mod block;
pub mod causes;
pub mod channel;
pub mod counted;
mod destination;
pub mod entry;
mod env;
mod exec;
mod exec_runtime;
mod fiber;
pub mod flow;
pub mod foreign;
mod growth;
pub mod host_file;
pub mod host_ops;
#[cfg(target_arch = "x86_64")]
mod isa;
pub mod isolate;
pub mod latest;
pub mod limit;
#[cfg(feature = "prof")]
mod mem_prof;
#[cfg(any(test, feature = "prof"))]
mod mem_sampler;
pub mod net;
pub mod os;
pub mod os_exec;
mod os_net;
mod os_resolve;
mod os_sync;
mod os_tls;
mod os_unix;
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
#[cfg(any(test, feature = "testing"))]
pub mod schedule_script;
mod scram;
pub mod secret;
mod sequencer;
mod sha1;
mod sha256;
mod signature;
#[cfg(all(feature = "standalone", not(test)))]
mod standalone_allocator;
mod stdin;
pub mod steps;
mod stop_signals;
#[cfg(feature = "record")]
pub mod tape;
mod task_heap;
pub mod tasks;
#[cfg(feature = "testing")]
pub mod test_provider;
pub mod tls;
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
#[repr(C)]
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
    pub(crate) run: *const crate::process::Config,
    pub(crate) map_hash_key: hirsel_core_layout::map_hash::PreparedKey,
    pub(crate) cancel_requested: AtomicBool,
    // Standalone roots and concurrent test tasks close their own heaps.
    owns_heap: bool,
    pub(crate) cancel_shield: usize,
    pub(crate) finalizing: usize,
    pub(crate) bound_depth: usize,
    pub(crate) primary: Option<Box<causes::Cell>>,
    pub(crate) causes: causes::Causes,
    pub(crate) task_record: Option<block::Block>,
    pub(crate) active_resource: *mut causes::Resource,
    pub(crate) host_files: *mut host_file::Files,
    pub(crate) isolates: Vec<crate::isolate::Reservation>,
    pub(crate) step_bounds: Vec<steps::Bound>,
    pub(crate) step_slice_tail: u64,
    pub(crate) regions: region::RegionCache,
    pub(crate) heap: std::cell::OnceCell<std::sync::Arc<task_heap::Heap>>,
    pub(crate) frame_lists: frame_list::FrameLists,
    pub(crate) spare_isolate_cell: Option<Box<std::mem::MaybeUninit<causes::Cell>>>,
    pub(crate) foreign: *const foreign::Run,
}

const _: () =
    assert!(std::mem::offset_of!(Context, map_hash_key) == abi_layout::CONTEXT_MAP_HASH_KEY);

impl Context {
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

    pub(crate) fn share_heap(&mut self, heap: std::sync::Arc<task_heap::Heap>) {
        self.heap = std::cell::OnceCell::from(heap);
        self.owns_heap = false;
    }

    pub(crate) fn with_funding(current: Funding) -> Self {
        current.normalize();
        Self {
            current,
            events: 0,
            steps: steps::UNBOUNDED,
            step_bounds: Vec::new(),
            step_slice_tail: steps::SLICE,
            regions: region::RegionCache::default(),
            heap: std::cell::OnceCell::new(),
            run: std::ptr::null(),
            map_hash_key: hirsel_core_layout::map_hash::PreparedKey::new(&[0; 16]),
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
        }
    }

    #[cfg(feature = "testing")]
    pub fn reset_test_limit(&mut self, bytes: u64) {
        self.current = Funding::root(bytes);
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
                .unwrap_or_else(|| process::stop(b"HostHandles"))
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
mod provider_s0_tests;
#[cfg(test)]
mod provider_tests;

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

#[cfg(feature = "prof")]
#[unsafe(no_mangle)]
pub static hirsel_core_prof_runtime: u8 = 1;
