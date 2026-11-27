use super::{Core, Host, Machine, Report, WorldInput, run_world, work_tally};
use hirsel_core_ir::{FuncId, Module, RtOp};
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

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// The map hashing and placement operations (`docs/internal/map-index.md`).
    pub(super) fn map_op(&mut self, op: RtOp, a: &[u64]) -> Vec<u64> {
        match op {
            RtOp::MapHashBytes => {
                work_tally::hashed(a[1]);
                let bytes = if a[1] == 0 {
                    &[]
                } else {
                    self.get(a[0], a[1] as usize)
                };
                vec![hirsel_core_layout::map_hash::bytes(
                    &self.maps.hash_key,
                    bytes,
                )]
            }
            RtOp::MapHashWord => vec![hirsel_core_layout::map_hash::word(
                &self.maps.hash_key,
                a[0],
            )],
            RtOp::MapHashStart => {
                let state = hirsel_core_layout::map_hash::State::new(&self.maps.hash_key);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapPlaceWord => {
                #[cfg(feature = "map-observation")]
                if let Some(stats) = &mut self.maps.observed {
                    stats.last_word_strong = false;
                }
                vec![hirsel_core_layout::map_index::fast_word(
                    a[0],
                    self.maps.fast_salt[0],
                )]
            }
            RtOp::MapPlaceBytes | RtOp::MapStrongBytes => {
                work_tally::hashed(a[1]);
                let bytes = if a[1] == 0 {
                    &[]
                } else {
                    self.get(a[0], a[1] as usize)
                };
                let [s0, s1] = self.maps.fast_salt;
                vec![if op == RtOp::MapPlaceBytes {
                    hirsel_core_layout::map_index::fast_bytes(bytes, s0, s1)
                } else {
                    hirsel_core_layout::map_hash::bytes(&self.maps.strong_key, bytes)
                }]
            }
            RtOp::MapStrongWord => {
                #[cfg(feature = "map-observation")]
                if let Some(stats) = &mut self.maps.observed {
                    stats.last_word_strong = true;
                }
                vec![hirsel_core_layout::map_hash::word(
                    &self.maps.strong_key,
                    a[0],
                )]
            }
            RtOp::MapStrongStart => {
                let state = hirsel_core_layout::map_hash::State::new(&self.maps.strong_key);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashAddWord => {
                let mut state = self.load_map_hash(a[0]);
                state.word(a[1]);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashAddBytes => {
                work_tally::hashed(a[2]);
                let mut state = self.load_map_hash(a[0]);
                let bytes = if a[2] == 0 {
                    &[]
                } else {
                    self.get(a[1], a[2] as usize)
                };
                state.bytes(bytes);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashFinish => vec![self.load_map_hash(a[0]).finish()],
            _ => unreachable!("not a map operation: {op:?}"),
        }
    }

    fn load_map_hash(&self, p: u64) -> hirsel_core_layout::map_hash::State {
        hirsel_core_layout::map_hash::State(std::array::from_fn(|i| self.load(p + i as u64 * 8, 8)))
    }

    fn store_map_hash(&mut self, p: u64, state: hirsel_core_layout::map_hash::State) {
        for (i, word) in state.0.into_iter().enumerate() {
            self.store(p + i as u64 * 8, 8, word);
        }
    }
}
