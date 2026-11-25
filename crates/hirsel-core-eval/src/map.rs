use super::{Core, Report, WorldInput, run_world};
use hirsel_core_ir::{FuncId, Module};
use std::{collections::hash_map::RandomState, hash::BuildHasher};

#[cfg(feature = "map-observation")]
use super::Checked;

pub(super) fn fresh_key() -> [u8; 16] {
    let seed = RandomState::new();
    let mut key = [0; 16];
    key[..8].copy_from_slice(&seed.hash_one(0u8).to_le_bytes());
    key[8..].copy_from_slice(&seed.hash_one(1u8).to_le_bytes());
    key
}

/// Test a single program with a chosen run key, and report its source step count.
pub fn run_map_hash(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    key: [u8; 16],
) -> (Report, u64) {
    let (report, checked) = run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            core: Core {
                map_hash_key: Some(key),
                ..Core::default()
            },
            ..WorldInput::default()
        },
    );
    (report, checked.steps)
}

/// Observe the probe and rebuild paths of the emitted CoreIR in a fixed-key run.
#[cfg(feature = "map-observation")]
pub fn run_map_observed(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    key: [u8; 16],
) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            core: Core {
                map_hash_key: Some(key),
                observe_maps: true,
                ..Core::default()
            },
            events: true,
            ..WorldInput::default()
        },
    )
}
