"""Emit loss_vector's tables (bwe_laws.hirsel) by running str0m's LossController.

The oracle is str0m 2ada76a's src/bwe/loss_controller.rs, libwebrtc's
LossBasedBweV2 in f64. This script clones str0m at that commit into /tmp
(never into the repo), builds the file standalone with its time.rs and
crates/proto/src/bandwidth.rs, test modules removed and log macros shimmed,
runs the phases below and prints the three tables. Needs git and cargo.

    python3 core/programs/rtp/vectors/loss_vector.py
"""

import subprocess
import sys
from pathlib import Path

STR0M = 'https://github.com/algesten/str0m.git'
COMMIT = '2ada76adfd4213f20169b23fc284b56ff37823d5'
REF = Path('/tmp/ref-str0m-' + COMMIT[:7])
BUILD = Path('/tmp/str0m-loss-vector')

LIB = r'''#![allow(dead_code, unused_macros, unused_imports)]
macro_rules! debug { ($($t:tt)*) => { { let _ = format_args!($($t)*); } } }
macro_rules! trace { ($($t:tt)*) => { { let _ = format_args!($($t)*); } } }
macro_rules! warn { ($($t:tt)*) => { { let _ = format_args!($($t)*); } } }
macro_rules! log_stat { ($($t:tt)*) => {} }
pub(crate) use log_stat;

mod bandwidth;
pub use bandwidth::{Bitrate, DataSize};

pub mod rtp_ {
    pub use crate::bandwidth::DataSize;
    use std::time::Instant;
    pub struct TwccSendRecord;
    impl TwccSendRecord {
        pub fn local_send_time(&self) -> Instant { unreachable!() }
        pub fn size(&self) -> usize { unreachable!() }
        pub fn remote_recv_time(&self) -> Option<Instant> { unreachable!() }
    }
}

pub mod bwe {
    pub use crate::bandwidth::Bitrate;
    pub(crate) mod macros {
        macro_rules! log_inherent_loss { ($($a:expr),+) => {} }
        macro_rules! log_loss_based_bitrate_estimate { ($($a:expr),+) => {} }
        macro_rules! log_loss_bw_limit_in_window { ($($a:expr),+) => {} }
        pub(crate) use log_inherent_loss;
        pub(crate) use log_loss_based_bitrate_estimate;
        pub(crate) use log_loss_bw_limit_in_window;
    }
    pub(crate) mod time;
    pub mod loss_controller;
}
'''

# Packet g (counting from 0 over the run) is sent in step k at
# k * 100 ms + j * 100 ms / n and is lost when (g * 7919) % 1000 < per mille.
# Phases: (steps, packets per step, bytes per packet, lost per mille,
# delay-based estimate bps, acked bps). The same table is VECTOR_PHASES.
HARNESS = r'''use super::*;
use std::time::{Duration, Instant};

struct P { t: Instant, size: DataSize, lost: bool }
impl PacketResult for P {
    fn local_send_time(&self) -> Instant { self.t }
    fn size(&self) -> DataSize { self.size }
    fn lost(&self) -> bool { self.lost }
}

const PHASES: [(u64, u64, u64, u64, u64, u64); 5] = [
    (20, 40, 1000, 10, 4_000_000, 1_500_000),
    (20, 40, 1000, 80, 4_000_000, 1_500_000),
    (20, 40, 1000, 300, 4_000_000, 1_500_000),
    (30, 40, 1000, 0, 4_000_000, 1_500_000),
    (20, 60, 1000, 30, 6_000_000, 2_000_000),
];

#[test]
fn vector() {
    let start = Instant::now();
    let mut lc = LossController::new();
    lc.set_min_bitrate(Bitrate::bps(30_000));
    lc.set_max_bitrate(Bitrate::bps(10_000_000));
    lc.set_bandwidth_estimate(Bitrate::bps(3_000_000));
    let (mut g, mut k) = (0u64, 0u64);
    for &(steps, n, size, permille, delay, acked) in PHASES.iter() {
        for _ in 0..steps {
            let mut pkts = vec![];
            for j in 0..n {
                let ns = k * 100_000_000 + j * (100_000_000 / n);
                let lost = (g * 7919) % 1000 < permille;
                pkts.push(P { t: start + Duration::from_nanos(ns), size: DataSize::from(size as usize), lost });
                g += 1;
            }
            lc.set_acknowledged_bitrate(Bitrate::bps(acked));
            lc.update_bandwidth_estimate(&pkts, Bitrate::bps(delay));
            let r = lc.loss_based_result();
            let code = match r.state {
                LossControllerState::Increasing => 0,
                LossControllerState::Decreasing => 1,
                LossControllerState::DelayBased => 2,
            };
            let est = r.bandwidth_estimate.map(|b| b.as_f64()).unwrap();
            eprintln!("ROW {} {} {:.3} {:.6}", k, code, est, lc.current_estimate.inherent_loss);
            k += 1;
        }
    }
}
'''


def without_tests(text):
    cut = text.find('\n#[cfg(test)]\nmod test')
    return text if cut < 0 else text[:cut + 1]


def main():
    if not REF.exists():
        subprocess.run(['git', 'clone', '-q', STR0M, str(REF)], check=True)
    subprocess.run(['git', '-C', str(REF), 'checkout', '-q', COMMIT], check=True)
    src = BUILD / 'src'
    (src / 'bwe').mkdir(parents=True, exist_ok=True)
    (BUILD / 'Cargo.toml').write_text('[package]\nname = "str0m-loss-vector"\nversion = "0.1.0"\nedition = "2021"\n')
    (src / 'lib.rs').write_text(LIB)
    (src / 'bandwidth.rs').write_text(without_tests((REF / 'crates/proto/src/bandwidth.rs').read_text()))
    (src / 'bwe/time.rs').write_text(without_tests((REF / 'src/bwe/time.rs').read_text()))
    loss = without_tests((REF / 'src/bwe/loss_controller.rs').read_text())
    (src / 'bwe/loss_controller.rs').write_text(loss + '\n#[cfg(test)]\n#[path = "../harness.rs"]\nmod harness;\n')
    (src / 'harness.rs').write_text(HARNESS)
    run = subprocess.run(['cargo', 'test', '-q', 'vector', '--', '--nocapture'], cwd=BUILD, capture_output=True, text=True)
    if run.returncode != 0:
        sys.exit(run.stdout + run.stderr)
    rows = [line.split()[1:] for line in run.stderr.splitlines() if line.startswith('ROW ')]
    states = [r[1] for r in rows]
    estimates = [str(int(float(r[2]))) for r in rows]
    inherent = [str(round(float(r[3]) * 65536)) for r in rows]
    n = len(rows)
    print(f'const VECTOR_STATES: Inline[u8, {n}] =[{", ".join(states)}]\n')
    print(f'const VECTOR_ESTIMATES: Inline[i64, {n}] =[{", ".join(estimates)}]\n')
    print(f'const VECTOR_INHERENT: Inline[i64, {n}] =[{", ".join(inherent)}]')


if __name__ == '__main__':
    main()
