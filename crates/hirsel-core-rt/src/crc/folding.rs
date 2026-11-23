//! Fold 128-bit lanes in reflected order, then reduce once and handle the tail.

use super::{NVME, zero_bit};

const fn shift_one(bits: usize) -> u64 {
    let mut rem = 1;
    let mut i = 0;
    while i < bits {
        rem = zero_bit(rem, NVME);
        i += 1;
    }
    rem
}

// A carry-less product contributes its low half first in reflected order.
// Moving a lane by n bytes applies the zero-bit map 8*n times to the low
// half and 8*n - 64 times to the high half, with 64 bits left to reduce.
const fn factors(bytes: usize) -> [u64; 2] {
    [shift_one(bytes * 8), shift_one(bytes * 8 - 64)]
}

const X16: [u64; 2] = factors(16);
const X32: [u64; 2] = factors(32);
const X48: [u64; 2] = factors(48);
const X64: [u64; 2] = factors(64);
#[cfg(target_arch = "x86_64")]
const X128: [u64; 2] = factors(128);

// The reversed generator is x^64 + REDUCTION_POLY. Its low 64 bits
// are odd, so it has an inverse modulo x^64. Multiplication by this
// inverse determines the quotient bits that cancel the low half.
const REDUCTION_POLY: u64 = (NVME << 1) | 1;
const INVERSE: u64 = inverse_mod_x64(REDUCTION_POLY);

const fn inverse_mod_x64(poly: u64) -> u64 {
    let mut residue = 1u64;
    let mut inverse = 0;
    let mut bit = 0;
    while bit < 64 {
        if residue & (1 << bit) != 0 {
            inverse |= 1 << bit;
            residue ^= poly << bit;
        }
        bit += 1;
    }
    inverse
}

// Four independent lanes hide multiply latency. Both architectures use
// the same lane layout and differ only in the vector operations.
macro_rules! fold_blocks {
    ($crc:expr, $bytes:expr) => {{
        let crc = $crc;
        let (groups, rest) = $bytes.as_chunks::<64>();
        if let Some((first, groups)) = groups.split_first() {
            let blocks = first.as_chunks::<16>().0;
            let mut a = xor(load(&blocks[0]), lanes([crc, 0]));
            let mut b = load(&blocks[1]);
            let mut c = load(&blocks[2]);
            let mut d = load(&blocks[3]);
            let factor = lanes(X64);
            for group in groups {
                let blocks = group.as_chunks::<16>().0;
                a = xor(fold(a, factor), load(&blocks[0]));
                b = xor(fold(b, factor), load(&blocks[1]));
                c = xor(fold(c, factor), load(&blocks[2]));
                d = xor(fold(d, factor), load(&blocks[3]));
            }
            let acc = xor(
                xor(fold(a, lanes(X48)), fold(b, lanes(X32))),
                xor(fold(c, lanes(X16)), d),
            );
            finish(acc, rest)
        } else if let Some((first, rest)) = $bytes.split_first_chunk::<16>() {
            finish(xor(load(first), lanes([crc, 0])), rest)
        } else {
            portable(crc, $bytes)
        }
    }};
}

#[cfg(target_arch = "x86_64")]
pub(super) mod x86 {
    use super::*;
    use crate::crc::portable;
    use std::arch::x86_64::*;

    #[target_feature(enable = "sse2")]
    fn lanes([lo, hi]: [u64; 2]) -> __m128i {
        _mm_set_epi64x(hi as i64, lo as i64)
    }

    #[target_feature(enable = "sse2")]
    fn load(block: &[u8; 16]) -> __m128i {
        // SAFETY: loadu reads the 16 initialized bytes of block at any alignment.
        unsafe { _mm_loadu_si128(block.as_ptr().cast()) }
    }

    #[target_feature(enable = "sse2")]
    fn xor(a: __m128i, b: __m128i) -> __m128i {
        _mm_xor_si128(a, b)
    }

    #[target_feature(enable = "pclmulqdq")]
    fn fold(a: __m128i, k: __m128i) -> __m128i {
        xor(
            _mm_clmulepi64_si128::<0>(a, k),
            _mm_clmulepi64_si128::<0x11>(a, k),
        )
    }

    #[target_feature(enable = "pclmulqdq")]
    fn finish(mut a: __m128i, rest: &[u8]) -> u64 {
        let (blocks, tail) = rest.as_chunks::<16>();
        for block in blocks {
            a = xor(fold(a, lanes(X16)), load(block));
        }
        let reduced = xor(
            _mm_srli_si128::<8>(a),
            _mm_clmulepi64_si128::<0x10>(a, lanes(X16)),
        );
        let q = _mm_clmulepi64_si128::<0>(reduced, lanes([INVERSE, 0]));
        let product = _mm_clmulepi64_si128::<0>(q, lanes([REDUCTION_POLY, 0]));
        let rem = xor(xor(reduced, _mm_slli_si128::<8>(q)), product);
        portable(_mm_cvtsi128_si64(_mm_srli_si128::<8>(rem)) as u64, tail)
    }

    #[target_feature(enable = "pclmulqdq")]
    pub(in crate::crc) unsafe fn folded(crc: u64, bytes: &[u8]) -> u64 {
        fold_blocks!(crc, bytes)
    }

    #[target_feature(enable = "avx2,vpclmulqdq")]
    fn fold_wide(a: __m256i, k: __m256i, block: &[u8; 32]) -> __m256i {
        // SAFETY: loadu reads the 32 initialized bytes of block at any alignment.
        let input = unsafe { _mm256_loadu_si256(block.as_ptr().cast()) };
        let lo = _mm256_clmulepi64_epi128::<0>(a, k);
        let hi = _mm256_clmulepi64_epi128::<0x11>(a, k);
        _mm256_xor_si256(_mm256_xor_si256(lo, hi), input)
    }

    #[target_feature(enable = "pclmulqdq,avx2,vpclmulqdq")]
    pub(in crate::crc) unsafe fn wide(crc: u64, bytes: &[u8]) -> u64 {
        let (groups, rest) = bytes.as_chunks::<128>();
        let Some((first, groups)) = groups.split_first() else {
            return fold_blocks!(crc, bytes);
        };
        let chunks = first.as_chunks::<32>().0;
        let zero = _mm256_setzero_si256();
        let seed = _mm256_inserti128_si256::<0>(zero, lanes([crc, 0]));
        let factor = _mm256_broadcastsi128_si256(lanes(X128));
        let mut a = fold_wide(zero, factor, &chunks[0]);
        a = _mm256_xor_si256(a, seed);
        let mut b = fold_wide(zero, factor, &chunks[1]);
        let mut c = fold_wide(zero, factor, &chunks[2]);
        let mut d = fold_wide(zero, factor, &chunks[3]);
        for group in groups {
            let chunks = group.as_chunks::<32>().0;
            a = fold_wide(a, factor, &chunks[0]);
            b = fold_wide(b, factor, &chunks[1]);
            c = fold_wide(c, factor, &chunks[2]);
            d = fold_wide(d, factor, &chunks[3]);
        }

        let k16 = lanes(X16);
        let k32 = lanes(X32);
        let mut acc = xor(
            fold(_mm256_castsi256_si128(a), k16),
            _mm256_extracti128_si256::<1>(a),
        );
        for pair in [b, c, d] {
            let lo = _mm256_castsi256_si128(pair);
            let hi = _mm256_extracti128_si256::<1>(pair);
            acc = xor(xor(fold(acc, k32), fold(lo, k16)), hi);
        }
        finish(acc, rest)
    }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
pub(super) mod arm {
    use super::*;
    use crate::crc::portable;
    use std::arch::aarch64::*;

    #[target_feature(enable = "aes")]
    fn lanes([lo, hi]: [u64; 2]) -> poly64x2_t {
        vreinterpretq_p64_u64(vcombine_u64(vcreate_u64(lo), vcreate_u64(hi)))
    }

    #[target_feature(enable = "aes")]
    fn load(block: &[u8; 16]) -> poly64x2_t {
        // SAFETY: vld1q_u8 reads the 16 initialized bytes of block.
        unsafe { vreinterpretq_p64_u8(vld1q_u8(block.as_ptr())) }
    }

    #[target_feature(enable = "aes")]
    fn xor(a: poly64x2_t, b: poly64x2_t) -> poly64x2_t {
        vreinterpretq_p64_u64(veorq_u64(
            vreinterpretq_u64_p64(a),
            vreinterpretq_u64_p64(b),
        ))
    }

    #[target_feature(enable = "aes")]
    fn fold(a: poly64x2_t, k: poly64x2_t) -> poly64x2_t {
        let lo = vmull_p64(vgetq_lane_p64::<0>(a), vgetq_lane_p64::<0>(k));
        let hi = vmull_high_p64(a, k);
        xor(vreinterpretq_p64_p128(lo), vreinterpretq_p64_p128(hi))
    }

    #[target_feature(enable = "aes")]
    fn finish(mut a: poly64x2_t, rest: &[u8]) -> u64 {
        let (blocks, tail) = rest.as_chunks::<16>();
        for block in blocks {
            a = xor(fold(a, lanes(X16)), load(block));
        }
        let [lo, hi] = [vgetq_lane_p64::<0>(a), vgetq_lane_p64::<1>(a)];
        let reduced = vmull_p64(lo, X16[1]) ^ u128::from(hi);
        let q = vmull_p64(reduced as u64, INVERSE) as u64;
        let rem = reduced ^ (u128::from(q) << 64) ^ vmull_p64(q, REDUCTION_POLY);
        portable((rem >> 64) as u64, tail)
    }

    #[target_feature(enable = "aes")]
    pub(in crate::crc) unsafe fn folded(crc: u64, bytes: &[u8]) -> u64 {
        fold_blocks!(crc, bytes)
    }
}
