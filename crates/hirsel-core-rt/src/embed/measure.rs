//! Opt-in scheduler observations for the Apple qualification fixture.
use crate::tasks::{Event, SchedulerProvider};
use hirsel_core_layout::event::{END, SPAWN};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

static ENABLED: AtomicBool = AtomicBool::new(false);
static LIVE_FIBERS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_harness_measure_fibers(enable: u32) {
    if enable != 0 {
        LIVE_FIBERS.store(0, Relaxed);
    }
    ENABLED.store(enable != 0, Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_harness_live_fibers() -> u64 {
    LIVE_FIBERS.load(Relaxed)
}

pub(crate) fn provider() -> Option<SchedulerProvider> {
    ENABLED.load(Relaxed).then(|| {
        // The configured entry starts with one root fiber. The scheduler records
        // the fixture's additional fibers through spawn and end transitions.
        LIVE_FIBERS.fetch_add(1, Relaxed);
        SchedulerProvider {
            event: Some(fiber_event),
            ..SchedulerProvider::default()
        }
    })
}

unsafe extern "C" fn fiber_event(_: *mut u8, event: Event) {
    match event.kind {
        SPAWN => {
            LIVE_FIBERS.fetch_add(1, Relaxed);
        }
        END => {
            LIVE_FIBERS.fetch_sub(1, Relaxed);
        }
        _ => {}
    }
}
