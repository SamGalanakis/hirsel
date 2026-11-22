// Rust twin of std.tally's measured case (core/twins/tally/tally.hirsel): four threads
// each add 1 N times into one u64 total, which is printed.
// `tally atomic N`: one shared AtomicU64, fetch_add per add.
// `tally local N`: a per-thread u64, the partials summed after the joins.
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("usage: tally atomic|local N");
    let n: u64 = args
        .next()
        .and_then(|n| n.parse().ok())
        .expect("usage: tally atomic|local N");
    let total = match mode.as_str() {
        "atomic" => {
            let total = AtomicU64::new(0);
            thread::scope(|s| {
                for _ in 0..4 {
                    s.spawn(|| {
                        for _ in 0..n {
                            total.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            });
            total.into_inner()
        }
        "local" => thread::scope(|s| {
            let parts: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(move || {
                        let mut part = 0u64;
                        for _ in 0..n {
                            part += 1;
                        }
                        part
                    })
                })
                .collect();
            parts.into_iter().map(|p| p.join().expect("worker")).sum()
        }),
        _ => panic!("usage: tally atomic|local N"),
    };
    println!("{total}");
}
