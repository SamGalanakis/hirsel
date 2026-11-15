//! Rust twin of the fault library's isolation of a bounds failure (M4-PLAN §4,
//! "fault (isolate)"): N bodies, each run under `catch_unwind` with a silent
//! panic hook. Every body allocates an owned buffer (dropped by the unwind, as
//! Hirsel's FAIL-8 cleanup releases a body's resources), then indexes a
//! 1000-element table past its end, so every body panics with a bounds
//! failure and every caught failure is counted.
//!
//! `m4-isolate N`: prints the failure count, which must be N, and a checksum
//! of the table. The per-isolation cost is the difference of runs at N and 0,
//! divided by N.
use std::hint::black_box;
use std::panic::{self, AssertUnwindSafe};

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .expect("usage: m4-isolate N")
        .parse()
        .unwrap();
    panic::set_hook(Box::new(|_| {}));
    let table: Vec<u64> = (0..1000).collect();
    let mut failed = 0u64;
    for i in 0..n {
        let at = black_box(1000 + (i % 7) as usize);
        let caught = panic::catch_unwind(AssertUnwindSafe(|| {
            let scratch = black_box(vec![i; 4]);
            table[at] + scratch[0]
        }));
        match caught {
            Err(_) => failed += 1,
            Ok(v) => panic!("index {at} of 1000 returned {v}"),
        }
    }
    assert_eq!(failed, n);
    println!("{failed} {}", table.iter().sum::<u64>());
}
