//! Opt-in receive-mode comparison counters. The protocol never reads them.

use std::sync::{
    OnceLock,
    atomic::{AtomicU8, AtomicU64, Ordering::Relaxed},
};

const BINS: usize = 65_536;
const WIDTH: u64 = 1_000;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("HIRSEL_UDP_MEASURE_GROUP").as_deref() == Ok("1"))
}

pub(crate) fn clock() -> u64 {
    if !enabled() {
        return 0;
    }
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a single writable timespec for the host monotonic clock.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    assert_eq!(result, 0);
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

pub(crate) struct Group {
    pub full: AtomicU64,
    pub dropped: AtomicU64,
    pub capture_discard: AtomicU64,
    placement: Box<[AtomicU8]>,
    delay: Box<[AtomicU64]>,
    overflow: AtomicU64,
}

impl Group {
    pub(crate) fn new(members: usize) -> Option<Self> {
        enabled().then(|| Self {
            full: AtomicU64::default(),
            dropped: AtomicU64::default(),
            capture_discard: AtomicU64::default(),
            placement: (0..members).map(|_| AtomicU8::default()).collect(),
            delay: (0..BINS).map(|_| AtomicU64::default()).collect(),
            overflow: AtomicU64::default(),
        })
    }

    pub(crate) fn delivered(&self, member: usize, home: bool, captured_at: u64) {
        let elapsed = (clock() - captured_at) / WIDTH;
        if let Some(bin) = self.delay.get(elapsed as usize) {
            bin.fetch_add(1, Relaxed);
        } else {
            self.overflow.fetch_add(1, Relaxed);
        }
        self.placement[member].fetch_or(if home { 1 } else { 2 }, Relaxed);
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let count = self.delay.iter().map(|bin| bin.load(Relaxed)).sum::<u64>();
        let [home, away, mixed] = [1, 2, 3].map(|mode| {
            self.placement
                .iter()
                .filter(|seen| seen.load(Relaxed) == mode)
                .count()
        });
        let percentile = |target| {
            let mut total = 0;
            for (bin, n) in self.delay.iter().enumerate() {
                total += n.load(Relaxed);
                if total >= target {
                    return (bin as u64 + 1) * WIDTH;
                }
            }
            0
        };
        eprintln!(
            "UDP_GROUP {count} {} {} {} {} {} {} {home} {away} {mixed}",
            self.full.load(Relaxed),
            self.dropped.load(Relaxed),
            self.capture_discard.load(Relaxed),
            self.overflow.load(Relaxed),
            percentile((count * 99).div_ceil(100)),
            percentile((count * 999).div_ceil(1000)),
        );
    }
}
