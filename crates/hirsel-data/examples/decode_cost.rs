//! Individual warm parse + decode samples and a separate live-allocation sample.
use hirsel_data::{Descriptor, Limits, decode};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
struct Meter;
hirsel_guard::install!(Meter);
static ON: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
fn add(n: usize) {
    CALLS.fetch_add(1, Relaxed);
    let used = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(used, Relaxed);
}
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if ON.load(Relaxed) && !p.is_null() {
            add(l.size())
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.load(Relaxed) {
            LIVE.fetch_sub(l.size(), Relaxed);
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let p = unsafe { System.realloc(p, l, n) };
        if ON.load(Relaxed) && !p.is_null() {
            LIVE.fetch_sub(l.size(), Relaxed);
            add(n)
        }
        p
    }
}
fn main() {
    let alloc = std::env::args().nth(1).as_deref() == Some("alloc");
    let schema = Descriptor::lower(
        "cost",
        "type Site { labels: Map[String, String], banner: String }",
        "Site",
    )
    .unwrap();
    for kib in [1, 16, 256] {
        let mut text = String::from("cost.Site(labels: [");
        let tail = "], banner: \"";
        for i in 0.. {
            let pair = format!("(\"entry{i}\", \"configuration value\"),");
            if text.len() + pair.len() + tail.len() + 2 > kib * 1024 {
                break;
            }
            text.push_str(&pair);
        }
        text.push_str(tail);
        text.extend(std::iter::repeat_n('x', kib * 1024 - 2 - text.len()));
        text.push_str("\")");
        let sample = || {
            std::hint::black_box(
                decode(
                    std::hint::black_box(text.as_bytes()),
                    &[&schema],
                    Limits::default(),
                )
                .unwrap(),
            )
        };
        drop(sample());
        if alloc {
            PEAK.store(0, Relaxed);
            CALLS.store(0, Relaxed);
            ON.store(true, Relaxed);
            drop(sample());
            ON.store(false, Relaxed);
            assert_eq!(LIVE.load(Relaxed), 0);
            println!(
                "{kib} KiB: {} allocations, {} peak live bytes",
                CALLS.load(Relaxed),
                PEAK.load(Relaxed)
            );
        } else {
            let mut times = Vec::new();
            for _ in 0..101 {
                let t = Instant::now();
                let doc = sample();
                times.push(t.elapsed().as_nanos());
                drop(doc);
            }
            times.sort_unstable();
            println!("{kib} KiB: p50 {} ns, p95 {} ns", times[50], times[95]);
        }
    }
}
