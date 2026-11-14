//! Optional MEM-19 balances and native backing observations. Shipping builds
//! omit the module and all hooks. One cooperative worker owns a run's samples;
//! a multi-worker scheduler must synchronize a run-owned observer first.
use crate::{Context, limit::Funding};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt::Write,
};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Figures {
    charged: u64,
    retained: u64,
    slack: u64,
}
impl Figures {
    fn include(&mut self, value: Self) {
        self.charged = self.charged.max(value.charged);
        self.retained = self.retained.max(value.retained);
        self.slack = self.slack.max(value.slack);
    }
    fn json(self, out: &mut String, prefix: &str) {
        write!(
            out,
            "\"{prefix}charged_bytes\":{},\"{prefix}retained_bytes\":{},\"{prefix}slack_bytes\":{}",
            self.charged, self.retained, self.slack
        )
        .unwrap();
    }
}
struct Phase {
    name: String,
    current: Figures,
    peak: Figures,
}
struct Region {
    pointer: *const crate::region::Region,
    tails: u64,
    tail: u64,
}
struct Observation {
    root: Funding,
    allowance: Option<u64>,
    current: Figures,
    peak: Figures,
    phase_peak: Figures,
    allocations: BTreeMap<usize, usize>,
    regions: BTreeMap<usize, Region>,
    phases: Vec<Phase>,
}
impl Observation {
    fn sample(&mut self) {
        self.current.charged = self
            .allowance
            .unwrap_or_else(|| self.root.observation_capacity())
            .wrapping_sub(self.root.available());
        self.peak.include(self.current);
        self.phase_peak.include(self.current);
    }
    fn refresh_regions(&mut self, borrowed: Option<usize>) {
        for (&key, r) in &mut self.regions {
            if Some(key) != borrowed {
                // SAFETY: opaque ABI regions stay pinned and live until removal.
                // Emitted checkpoints have no Rust region borrow in progress;
                // a runtime region method supplies its own tail instead of aliasing it.
                let region = unsafe { &*r.pointer };
                r.tail = (region.end.addr() - region.cursor.addr()) as u64;
            }
        }
        self.current.slack = self.regions.values().map(|r| r.tails + r.tail).sum();
    }
    fn checkpoint(&mut self, name: &str) {
        self.refresh_regions(None);
        self.sample();
        self.phases.push(Phase {
            name: name.into(),
            current: self.current,
            peak: self.phase_peak,
        });
        self.phase_peak = self.current;
    }
    fn json(&self) -> String {
        let mut out = format!(
            "{{\"observation_mode\":true,\"metric_version\":2,\"peak_root_reserved_bytes\":{},",
            self.peak.charged
        );
        self.current.json(&mut out, "");
        out.push(',');
        self.peak.json(&mut out, "peak_");
        out.push_str(",\"charged_scope\":\"MEM-19 root reservations including bounds/tasks/grants/startup\",\"retained_scope\":\"live backing requests, not RSS or freed allocator arenas\",\"slack_scope\":\"unassigned region chunk tails, not capacity-length or trim waste\",\"phases\":[");
        for (i, phase) in self.phases.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            json_string(&mut out, &phase.name);
            out.push(',');
            phase.current.json(&mut out, "");
            out.push(',');
            phase.peak.json(&mut out, "peak_");
            out.push('}');
        }
        out.push_str("]}\n");
        out
    }
}
fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}
thread_local! {
    static ACTIVE: RefCell<Vec<Observation>> = const { RefCell::new(Vec::new()) };
    static LAST: Cell<u64> = const { Cell::new(0) };
    static LAST_REPORT: RefCell<String> = const { RefCell::new(String::new()) };
}
/// Retains the original root through forwarding; restores nested host observations.
pub struct Run(std::marker::PhantomData<std::rc::Rc<()>>);
impl Run {
    pub fn start(context: &Context) -> Self {
        Self::begin(context, None)
    }
    /// Sample fixtures exclude prepaid process startup and close this interval
    /// before releasing it. Shipping reports always observe the complete root.
    #[cfg(feature = "testing")]
    pub fn start_test_interval(context: &Context) -> Self {
        Self::begin(context, Some(context.available()))
    }
    fn begin(context: &Context, allowance: Option<u64>) -> Self {
        ACTIVE.with(|active| {
            active.borrow_mut().push(Observation {
                root: context.current.clone(),
                allowance,
                current: Figures::default(),
                peak: Figures::default(),
                phase_peak: Figures::default(),
                allocations: BTreeMap::new(),
                regions: BTreeMap::new(),
                phases: Vec::new(),
            })
        });
        LAST.set(0);
        LAST_REPORT.with(|report| report.borrow_mut().clear());
        checkpoint("run_start");
        Self(std::marker::PhantomData)
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let mut observation = active.borrow_mut().pop().unwrap();
            observation.checkpoint("run_end");
            LAST.set(observation.peak.charged);
            LAST_REPORT.with(|report| *report.borrow_mut() = observation.json());
        });
    }
}
#[inline]
pub(crate) fn sample() {
    ACTIVE.with(|active| {
        if let Some(o) = active.borrow_mut().last_mut() {
            o.sample();
        }
    });
}
pub(crate) fn checkpoint(name: &str) {
    ACTIVE.with(|active| {
        if let Some(o) = active.borrow_mut().last_mut() {
            o.checkpoint(name);
        }
    });
}
pub(crate) fn allocated(pointer: *const u8, bytes: usize) {
    ACTIVE.with(|active| {
        if let Some(o) = active.borrow_mut().last_mut() {
            o.allocations.insert(pointer.addr(), bytes);
            o.current.retained += bytes as u64;
            o.sample();
        }
    });
}
pub(crate) fn deallocated(pointer: *const u8) {
    ACTIVE.with(|active| {
        // A nested host run can release backing allocated by an enclosing run.
        // Attribute that free to its owning observation, not the current one.
        for o in active.borrow_mut().iter_mut().rev() {
            if let Some(bytes) = o.allocations.remove(&pointer.addr()) {
                o.current.retained -= bytes as u64;
                o.sample();
                break;
            }
        }
    });
}
/// # Safety
/// ABI region is pinned until Drop; its cursor/end remain live at each sample.
pub(crate) unsafe fn region_open(pointer: *const crate::region::Region) {
    ACTIVE.with(|active| {
        if let Some(o) = active.borrow_mut().last_mut() {
            o.regions.insert(
                pointer.addr(),
                Region {
                    pointer,
                    tails: 0,
                    tail: 0,
                },
            );
            o.sample();
        }
    });
}
pub(crate) fn region_refill(pointer: *const crate::region::Region, old_tail: usize, tail: usize) {
    ACTIVE.with(|active| {
        for o in active.borrow_mut().iter_mut().rev() {
            if let Some(r) = o.regions.get_mut(&pointer.addr()) {
                r.tails += old_tail as u64;
                r.tail = tail as u64;
                o.refresh_regions(Some(pointer.addr()));
                o.sample();
                break;
            }
        }
    });
}
pub(crate) fn region_close(pointer: *const crate::region::Region) {
    ACTIVE.with(|active| {
        for o in active.borrow_mut().iter_mut().rev() {
            if o.regions.remove(&pointer.addr()).is_some() {
                o.refresh_regions(None);
                o.sample();
                break;
            }
        }
    });
}
/// Samples an emitted debit. No status/admission/refund delivery/event.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_observe_charge() {
    ACTIVE.with(|active| {
        if let Some(o) = active.borrow_mut().last_mut() {
            o.refresh_regions(None);
            o.sample();
        }
    });
}
/// Last completed run's root peak reservation, including bound/task/grant reserves.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_peak_charge() -> u64 {
    LAST.get()
}
/// Borrow the last completed report's UTF-8 bytes until an observed run next
/// starts or completes on this worker. No admission/event/failure status. `len` is an out pointer.
/// # Safety
/// `len` points to writable usize storage; caller does not mutate the returned bytes.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_memory_report(len: *mut usize) -> *const u8 {
    LAST_REPORT.with(|report| {
        let report = report.borrow();
        let text = if report.is_empty() {
            EMPTY_REPORT
        } else {
            &report
        };
        // SAFETY: caller supplies the writable length destination.
        unsafe {
            *len = text.len();
        }
        text.as_ptr()
    })
}
#[cfg(feature = "charge-observation")]
const EMPTY_REPORT: &str = "{\"observation_mode\":true,\"metric_version\":2,\"peak_root_reserved_bytes\":0,\"charged_bytes\":0,\"retained_bytes\":0,\"slack_bytes\":0,\"peak_charged_bytes\":0,\"peak_retained_bytes\":0,\"peak_slack_bytes\":0,\"phases\":[]}\n";
/// Record a native harness phase endpoint and start the next peak interval.
/// No language operation, admission, refund delivery or scheduler event.
/// # Safety
/// `name` borrows `len` readable UTF-8 bytes for the duration of this call.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_memory_checkpoint(name: *const u8, len: usize) {
    // SAFETY: the native harness provides a live UTF-8 label view.
    let name = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, len)) };
    checkpoint(name);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peak_counts_reservations_and_keeps_survivor_root_after_rank_merge() {
        let mut context = Context::new(1000);
        let run = Run::start(&context);
        context.limit_enter(600).unwrap();
        context.current.admit(Some(40)).unwrap();
        let survivor = context.current.payer;
        context.limit_enter(100).unwrap();
        context.current.admit(Some(10)).unwrap();
        let nested = context.current.payer;
        context.limit_exit();
        context.limit_exit();
        assert_eq!(context.available(), 950);
        nested.refund(10);
        survivor.refund(40);
        context.current.admit(Some(80)).unwrap();
        context.current.refund(80);
        assert_eq!(context.available(), 1000);
        drop(run);
        assert_eq!(LAST.get(), 600);
    }
    #[test]
    fn refusal_does_not_raise_peak_and_consecutive_runs_reset_it() {
        for (bound, expected) in [(1024, 0), (200, 200), (0, 0)] {
            let mut context = Context::new(1000);
            let run = Run::start(&context);
            if context.limit_enter(bound).is_ok() {
                context.limit_exit();
            }
            drop(run);
            assert_eq!(LAST.get(), expected);
        }
    }
    #[test]
    fn inline_sample_and_nested_host_run_preserve_outer_peak() {
        let mut outer = Context::new(1000);
        let outer_run = Run::start(&outer);
        // SAFETY: models the emitter's task-exclusive frozen-prefix stores.
        unsafe {
            (*outer.limit_record()).available -= 80;
            (*outer.limit_record()).outstanding += 80;
        }
        sample();
        {
            let inner = Context::new(200);
            let inner_run = Run::start(&inner);
            inner.current.admit(Some(100)).unwrap();
            inner.current.refund(100);
            drop(inner_run);
            assert_eq!(LAST.get(), 100);
        }
        outer.current.refund(80);
        outer.limit_enter(50).unwrap();
        outer.limit_exit();
        drop(outer_run);
        assert_eq!(LAST.get(), 80);
    }
    fn figures() -> Figures {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let o = active.last_mut().unwrap();
            o.refresh_regions(None);
            o.sample();
            o.current
        })
    }
    #[test]
    fn charged_is_root_reservation_and_retained_is_live_native_requests() {
        let mut context = Context::new(1000);
        let run = Run::start(&context);
        let plain = crate::block::Block::new(&context, Some(24), 17, 8).unwrap();
        let graph = crate::block::Block::graph(&context, Some(24), 17, 8).unwrap();
        assert_eq!(
            figures(),
            Figures {
                charged: 48,
                retained: 25 + 65,
                slack: 0
            }
        );
        context.limit_enter(200).unwrap();
        let survivor = crate::block::Block::new(&context, Some(24), 17, 8).unwrap();
        assert_eq!(figures().charged, 248);
        assert_eq!(figures().retained, 115);
        context.limit_exit();
        assert_eq!(figures().charged, 72);
        drop((plain, graph, survivor));
        assert_eq!(figures(), Figures::default());
        drop(run);
        assert_eq!(LAST.get(), 248);
    }
    #[test]
    fn region_slack_is_chunk_tails_and_close_releases_all_backing() {
        let context = Context::new(1000);
        let run = Run::start(&context);
        let region = crate::abi::hirsel_core_region_open(128);
        let metadata = size_of::<crate::region::Region>() as u64;
        // SAFETY: the opaque pinned ABI region is live until close. All queried
        // checkpoints occur after its exclusive allocation call has returned.
        unsafe {
            assert!(
                !crate::abi::hirsel_core_region_alloc(&context, region, 8, 8, 8, false)
                    .pointer
                    .is_null()
            );
            assert_eq!(
                figures(),
                Figures {
                    charged: 8,
                    retained: metadata + 128,
                    slack: 120
                }
            );
            let entry = (*region).entry;
            crate::abi::hirsel_core_region_lower_charge(region, entry, 8, 0);
            assert_eq!(
                figures(),
                Figures {
                    charged: 0,
                    retained: metadata + 128,
                    slack: 120
                }
            );
            assert!(
                !crate::abi::hirsel_core_region_alloc(&context, region, 24, 8, 24, false)
                    .pointer
                    .is_null()
            );
            assert_eq!(figures().slack, 96);
            assert!(
                !crate::abi::hirsel_core_region_alloc(&context, region, 200, 8, 200, false)
                    .pointer
                    .is_null()
            );
            assert_eq!(
                figures(),
                Figures {
                    charged: 224,
                    retained: metadata + 328,
                    slack: 96
                }
            );
            crate::abi::hirsel_core_region_close(region);
        }
        assert_eq!(figures(), Figures::default());
        drop(run);
    }
    #[test]
    fn phase_peaks_reset_and_report_keeps_exact_labels_and_end_figures() {
        let context = Context::new(1000);
        let run = Run::start(&context);
        let block = crate::block::Block::new(&context, Some(80), 80, 8).unwrap();
        checkpoint("build\n\"λ\"");
        drop(block);
        checkpoint("released");
        assert_eq!(figures(), Figures::default());
        drop(run);
        LAST_REPORT.with(|json| {
            let json = json.borrow();
            assert!(json.contains("\"peak_root_reserved_bytes\":80"));
            assert!(json.contains("\"name\":\"build\\n\\\"λ\\\"\""));
            assert!(json.contains("\"name\":\"run_end\",\"charged_bytes\":0,\"retained_bytes\":0,\"slack_bytes\":0,\"peak_charged_bytes\":0,\"peak_retained_bytes\":0,\"peak_slack_bytes\":0"));
        });
        let next = Run::start(&context);
        drop(next);
        assert_eq!(LAST.get(), 0);
    }
    #[test]
    fn prepaid_cells_resource_state_and_task_storage_are_retained_backing() {
        let context = Context::new(1000);
        let run = Run::start(&context);
        context.current.admit(Some(40)).unwrap();
        let storage = crate::tasks::Storage::new(33, 8);
        assert_eq!(
            figures(),
            Figures {
                charged: 40,
                retained: 33,
                slack: 0
            }
        );
        drop(storage);
        context.current.refund(40);
        // SAFETY: wholly destructure the fresh resource; no finalizer or fields
        // have been installed, and its complete grant/cell are unused.
        unsafe {
            let resource = crate::causes::resource_new(&context, 16, 64, 16, 8).unwrap();
            assert_eq!(figures().charged, 16 + 32 + 64);
            assert_eq!(
                figures().retained,
                32 + size_of::<crate::causes::Cell>() as u64
                    + size_of::<crate::causes::Resource>() as u64
            );
            crate::causes::resource_free(resource, 16);
        }
        assert_eq!(figures(), Figures::default());
        drop(run);
    }
    #[test]
    fn nested_run_refunds_backing_to_its_original_observation() {
        let outer = Context::new(1000);
        let outer_run = Run::start(&outer);
        let block = crate::block::Block::new(&outer, Some(40), 40, 8).unwrap();
        assert_eq!(figures().retained, 48);
        let inner = Context::new(200);
        let inner_run = Run::start(&inner);
        drop(block);
        assert_eq!(figures(), Figures::default());
        drop(inner_run);
        assert_eq!(figures(), Figures::default());
        drop(outer_run);
        assert_eq!(LAST.get(), 40);
    }
    #[test]
    fn nested_checkpoint_can_close_a_preexisting_pinned_region() {
        let context = Context::new(1000);
        let outer = Run::start(&context);
        let region = crate::abi::hirsel_core_region_open(128);
        // SAFETY: this native handle remains pinned through its unique close.
        unsafe {
            crate::abi::hirsel_core_region_alloc(&context, region, 8, 8, 8, false);
        }
        let inner = Run::start(&context);
        assert_eq!(figures().charged, 8);
        // SAFETY: no region mutation is in progress; the same task still owns it.
        unsafe {
            crate::abi::hirsel_core_region_close(region);
        }
        drop(inner);
        assert_eq!(figures(), Figures::default());
        drop(outer);
    }
}
