//! The two virtual Random sources shared by the native runtime and evaluator.

use std::mem::MaybeUninit;
use zeroize::Zeroizing;

pub enum RandomStream {
    Seeded(u64),
    Tape {
        bytes: Zeroizing<Vec<u8>>,
        cursor: usize,
    },
}

impl RandomStream {
    pub fn tape(bytes: Vec<u8>) -> Self {
        Self::Tape {
            bytes: Zeroizing::new(bytes),
            cursor: 0,
        }
    }

    /// Fill the entire destination, or leave the cursor and output untouched.
    pub fn draw(&mut self, out: &mut [MaybeUninit<u8>]) -> bool {
        match self {
            Self::Seeded(seed) => {
                for chunk in out.chunks_mut(8) {
                    chunk.write_copy_of_slice(&split_mix(seed)[..chunk.len()]);
                }
            }
            Self::Tape { bytes, cursor } => {
                let Some(draw) = bytes[*cursor..].get(..out.len()) else {
                    return false;
                };
                out.write_copy_of_slice(draw);
                *cursor += draw.len();
            }
        }
        true
    }
}

/// SplitMix64 (Steele, Lea and Flood 2014), eight little-endian bytes a step.
fn split_mix(seed: &mut u64) -> [u8; 8] {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut word = *seed;
    word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (word ^ (word >> 31)).to_le_bytes()
}
