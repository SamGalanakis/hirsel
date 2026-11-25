//! Bounded sampling of unit dispatch and task queues in the testing archive.

use super::{Unit, current, now, worker_ix};
use std::cell::{Cell, RefCell};
use std::sync::LazyLock;
use std::sync::atomic::Ordering;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("HIRSEL_UNIT_OBSERVE").is_some());
const STRIDE: u64 = 32;
const MAX_SAMPLES: usize = 32768;

#[derive(Clone, Copy)]
pub(super) enum Queue {
    Private,
    Local,
    Injector,
    Stolen,
}

enum Event {
    Entry {
        ns: u64,
        unit: u32,
        worker: usize,
        wait_ns: u64,
        publisher_cpu: u32,
        entry_cpu: u32,
    },
    Depth {
        ns: u64,
        unit: u32,
        worker: usize,
        ready: u64,
    },
}

#[derive(Default)]
struct Observer {
    entries: u64,
    depth_points: u64,
    hits: [u64; 4],
    dropped: u64,
    samples: Vec<Event>,
}

thread_local! {
    static OBSERVER: RefCell<Observer> = RefCell::new(Observer::default());
    static REENTRANT: Cell<u64> = const { Cell::new(0) };
}

// The calling dispatcher can resume on a different OS thread after a
// fiber switch. Resolve TLS here, on the current worker, on every call.
#[inline(never)]
fn with_observer<T>(record: impl FnOnce(&mut Observer) -> T) -> Option<T> {
    OBSERVER.with(|observer| match observer.try_borrow_mut() {
        Ok(mut state) => Some(record(&mut state)),
        Err(_) => {
            REENTRANT.set(REENTRANT.get() + 1);
            None
        }
    })
}

pub(super) fn enabled() -> bool {
    *ENABLED
}

fn cpu() -> u32 {
    crate::host::sys::cpu()
}

pub(super) fn queued(unit: &Unit) {
    if enabled() {
        unit.observed_queue_cpu.store(cpu(), Ordering::Relaxed);
        unit.observed_queue_ns.store(now(), Ordering::Relaxed);
    }
}

fn save(observer: &mut Observer, event: Event) {
    if observer.samples.len() == MAX_SAMPLES {
        observer.dropped += 1;
    } else {
        observer.samples.push(event);
    }
}

pub(super) fn enter(unit: &Unit, worker: usize, ns: u64) {
    let queued_ns = unit.observed_queue_ns.load(Ordering::Relaxed);
    let publisher_cpu = unit.observed_queue_cpu.load(Ordering::Relaxed);
    let ordinal = with_observer(|state| {
        state.entries += 1;
        state.entries
    });
    if queued_ns != 0 && ordinal.is_some_and(|entry| entry % STRIDE == 1) {
        let event = Event::Entry {
            ns,
            unit: unit.id,
            worker,
            wait_ns: ns.saturating_sub(queued_ns),
            publisher_cpu,
            entry_cpu: cpu(),
        };
        with_observer(|state| save(state, event));
    }
}

pub(super) fn found(queue: Queue) {
    if enabled() {
        with_observer(|state| state.hits[queue as usize] += 1);
    }
}

pub(crate) fn sample_depth() -> bool {
    enabled()
        && current() != super::NONE
        && with_observer(|state| {
            state.depth_points += 1;
            state.depth_points % STRIDE == 1
        })
        .unwrap_or(false)
}

pub(crate) fn depth(ready: u64) {
    let event = Event::Depth {
        ns: now(),
        unit: current(),
        worker: worker_ix(),
        ready,
    };
    with_observer(|state| save(state, event));
}

pub(super) fn flush() {
    if !enabled() {
        return;
    }
    let Some(state) = with_observer(std::mem::take) else {
        return;
    };
    {
        use std::io::Write;
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unknown");
        let mut out = std::io::stderr().lock();
        let [private, local, injector, stolen] = state.hits;
        let _ = writeln!(
            out,
            "unit-observe worker={name} entries={} depth_points={} private={private} local={local} injector={injector} stolen={stolen} dropped={} reentrant={}",
            state.entries,
            state.depth_points,
            state.dropped,
            REENTRANT.get()
        );
        for event in &state.samples {
            match event {
                Event::Entry {
                    ns,
                    unit,
                    worker,
                    wait_ns,
                    publisher_cpu,
                    entry_cpu,
                } => {
                    let _ = writeln!(
                        out,
                        "unit-entry {ns} {unit} {worker} {wait_ns} {publisher_cpu} {entry_cpu}"
                    );
                }
                Event::Depth {
                    ns,
                    unit,
                    worker,
                    ready,
                } => {
                    let _ = writeln!(out, "unit-depth {ns} {unit} {worker} {ready}");
                }
            }
        }
    }
}
