//! The grouped Map index and placement functions (docs/internal/map-index.md).

use crate::map_hash::PreparedKey;
use crate::{Body, Carried, FieldTy, Repr, Scalar, Shape, Ty};

pub const MAX_CAPACITY: u64 = 1 << 59;
pub const WIDE_CAPACITY: u64 = 1 << 32;
pub const GROUP_WIDTH: usize = 16;
pub const FAST_GROUP_LIMIT: u64 = 8;
pub const EMPTY: u8 = 0xff;
pub const DELETED: u8 = 0x80;
pub const LIVE_BIT: u64 = 1 << 56;
pub const TAG_SHIFT: u32 = 57;
pub const STRONG_EMPTY: u64 = 1;

pub const LIVE_COUNT: u64 = 0;
pub const GROUP_MASK: u64 = 8;
pub const MODE: u64 = 16;
pub const HEADER_BYTES: u64 = 32;
pub const STRONG: u64 = 1;
pub const WIDE: u64 = 2;
pub const NARROW_GROUP_BYTES: u64 = 80;
pub const WIDE_GROUP_BYTES: u64 = 144;

pub const MIX: u64 = 0x9e3779b97f4a7c15;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Class {
    Word(Scalar),
    Bytes,
    Composite,
}

pub fn class(shape: &Shape) -> Class {
    if let Carried::Value(Ty::Scalar(scalar)) = shape.carried
        && !Ty::Scalar(scalar).is_float()
        && matches!(shape.body, Body::Record(_))
    {
        return Class::Word(scalar);
    }
    if !matches!(shape.repr, Repr::Region)
        && let Body::Record(fields) = &shape.body
        && let [field] = fields.as_slice()
        && matches!(&field.ty, FieldTy::Seq(item) if **item == FieldTy::Scalar(Scalar::U8))
    {
        return Class::Bytes;
    }
    Class::Composite
}

pub fn buckets(cap: u64) -> Option<u64> {
    if cap == 0 || cap > MAX_CAPACITY {
        return None;
    }
    Some((cap * 2).next_power_of_two().max(GROUP_WIDTH as u64))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexLayout {
    pub header: u64,
    pub groups: u64,
    pub group_bytes: u64,
    pub physical: u64,
}

impl IndexLayout {
    pub fn new(cap: u64, entry_stride: u64) -> Option<Self> {
        let groups = buckets(cap)? / GROUP_WIDTH as u64;
        let header = cap.checked_mul(entry_stride)?;
        let group_bytes = if cap > WIDE_CAPACITY {
            WIDE_GROUP_BYTES
        } else {
            NARROW_GROUP_BYTES
        };
        let physical = header
            .checked_add(HEADER_BYTES)?
            .checked_add(groups.checked_mul(group_bytes)?)?;
        Some(Self {
            header,
            groups,
            group_bytes,
            physical,
        })
    }
}

pub fn derive(key: &[u8; 16]) -> (u64, u64, [u8; 16]) {
    let key = PreparedKey::new(key);
    let mut strong = [0; 16];
    strong[..8].copy_from_slice(&key.raw(b"hirsel.map.strong.0").to_le_bytes());
    strong[8..].copy_from_slice(&key.raw(b"hirsel.map.strong.1").to_le_bytes());
    (
        key.raw(b"hirsel.map.fast.0"),
        key.raw(b"hirsel.map.fast.1"),
        strong,
    )
}

#[inline]
fn fold(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    product as u64 ^ (product >> 64) as u64
}

#[inline]
pub fn fast_word(word: u64, s0: u64) -> u64 {
    fold(word ^ s0, MIX)
}

#[inline]
pub fn fast_bytes(bytes: &[u8], s0: u64, s1: u64) -> u64 {
    let mut remaining = bytes;
    let mut z = s0;
    while remaining.len() > GROUP_WIDTH {
        let (group, rest) = remaining.split_first_chunk::<GROUP_WIDTH>().unwrap();
        let (low, high) = group.split_at(8);
        z = fold(
            z ^ u64::from_le_bytes(low.try_into().unwrap()),
            s1 ^ u64::from_le_bytes(high.try_into().unwrap()),
        );
        remaining = rest;
    }
    let (low, high) = remaining.split_at(remaining.len().min(8));
    let mut w0 = [0; 8];
    let mut w1 = [0; 8];
    w0[..low.len()].copy_from_slice(low);
    w1[..high.len()].copy_from_slice(high);
    let t = fold(
        z ^ u64::from_le_bytes(w0),
        (s1 ^ u64::from_le_bytes(w1)).wrapping_add(bytes.len() as u64),
    );
    fold(t, MIX)
}

const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
const LOW_BITS: u64 = !HIGH_BITS;

fn pack_high_bits(word: u64) -> u8 {
    ((word & HIGH_BITS).wrapping_mul(0x0002_0408_1020_4081) >> 56) as u8
}

fn swar_mask(group: &[u8; GROUP_WIDTH], high_bits: impl Fn(u64) -> u64) -> u16 {
    let (lo, hi) = group.split_at(8);
    let lo = pack_high_bits(high_bits(u64::from_le_bytes(lo.try_into().unwrap())));
    let hi = pack_high_bits(high_bits(u64::from_le_bytes(hi.try_into().unwrap())));
    u16::from_le_bytes([lo, hi])
}

pub fn match_swar(group: &[u8; GROUP_WIDTH], tag: u8) -> u16 {
    let repeated = u64::from_le_bytes([tag; 8]);
    swar_mask(group, |word| {
        let diff = word ^ repeated;
        // The sum in each byte is at most 254, so no carry changes a neighbor.
        !(((diff & LOW_BITS) + LOW_BITS) | diff)
    })
}

pub fn vacant_swar(group: &[u8; GROUP_WIDTH]) -> u16 {
    swar_mask(group, |word| word)
}

#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "x86", target_feature = "sse2")
))]
pub use sse2::{match_mask, vacant_mask};
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "x86", target_feature = "sse2")
))]
mod sse2 {
    use super::GROUP_WIDTH;
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    #[inline]
    pub fn match_mask(group: &[u8; GROUP_WIDTH], tag: u8) -> u16 {
        // SAFETY: SSE2 is available and the unaligned load reads the array.
        unsafe {
            let bytes = _mm_loadu_si128(group.as_ptr().cast());
            _mm_movemask_epi8(_mm_cmpeq_epi8(bytes, _mm_set1_epi8(tag as i8))) as u16
        }
    }

    #[inline]
    pub fn vacant_mask(group: &[u8; GROUP_WIDTH]) -> u16 {
        // SAFETY: SSE2 is available and the unaligned load reads the array.
        unsafe { _mm_movemask_epi8(_mm_loadu_si128(group.as_ptr().cast())) as u16 }
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
pub use neon::{match_mask, vacant_mask};
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use super::GROUP_WIDTH;
    use std::arch::aarch64::*;

    #[inline]
    fn pack_high_bits(bytes: uint8x16_t) -> u16 {
        let weights = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128u8];
        // SAFETY: NEON is available and the unaligned load reads the array.
        unsafe {
            let bits = vshrq_n_u8::<7>(bytes);
            let weighted = vmulq_u8(bits, vld1q_u8(weights.as_ptr()));
            u16::from_le_bytes([
                vaddv_u8(vget_low_u8(weighted)),
                vaddv_u8(vget_high_u8(weighted)),
            ])
        }
    }

    #[inline]
    pub fn match_mask(group: &[u8; GROUP_WIDTH], tag: u8) -> u16 {
        // SAFETY: NEON is available and the unaligned load reads the array.
        unsafe { pack_high_bits(vceqq_u8(vld1q_u8(group.as_ptr()), vdupq_n_u8(tag))) }
    }

    #[inline]
    pub fn vacant_mask(group: &[u8; GROUP_WIDTH]) -> u16 {
        // SAFETY: NEON is available and the unaligned load reads the array.
        unsafe { pack_high_bits(vld1q_u8(group.as_ptr())) }
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "x86", target_feature = "sse2"),
    all(target_arch = "aarch64", target_feature = "neon"),
)))]
pub use self::{match_swar as match_mask, vacant_swar as vacant_mask};
