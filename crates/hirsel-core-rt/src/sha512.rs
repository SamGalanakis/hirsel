//! FIPS 180-4 SHA-512 compression over 128-byte blocks and eight state words.
//! Set HIRSEL_SHA512=portable to disable the CPU instructions.

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use std::sync::OnceLock;

// FIPS 180-4 section 4.2.3.
const K: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn use_instructions() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("HIRSEL_SHA512").is_some_and(|v| v == "portable") {
            return false;
        }
        #[cfg(target_arch = "x86_64")]
        return std::is_x86_feature_detected!("sha512") && std::is_x86_feature_detected!("avx2");
        #[cfg(target_arch = "aarch64")]
        return std::arch::is_aarch64_feature_detected!("sha3");
    })
}

/// Compress `blocks` 128-byte blocks at `data` into the eight state words.
/// Zero blocks read nothing.
///
/// # Safety
/// For nonzero `blocks`, `state` must point to eight aligned, initialized,
/// writable `u64` words, and `data` to `128 × blocks` readable bytes.
/// The regions must not overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sha512_blocks(state: *mut u64, data: *const u8, blocks: u64) {
    if blocks == 0 {
        return;
    }
    // SAFETY: the caller provides both valid, disjoint regions.
    let (state, blocks) = unsafe {
        (
            &mut *state.cast::<[u64; 8]>(),
            std::slice::from_raw_parts(data.cast::<[u8; 128]>(), blocks as usize),
        )
    };
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    if use_instructions() {
        // SAFETY: dispatch requires the features of the target's instructions.
        unsafe { instructions::compress(state, blocks) };
        return;
    }
    portable(state, blocks);
}

fn schedule(block: &[u8; 128]) -> [u64; 80] {
    let mut w = [0u64; 80];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<8>().0) {
        *word = u64::from_be_bytes(*bytes);
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    w
}

fn portable(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
    for block in blocks {
        let mut v = *state;
        for (word, k) in schedule(block).into_iter().zip(K) {
            let [a, b, c, d, e, f, g, h] = v;
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k)
                .wrapping_add(word);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (word, update) in state.iter_mut().zip(v) {
            *word = word.wrapping_add(update);
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod instructions {
    use super::{K, schedule};
    use std::arch::x86_64::*;

    #[target_feature(enable = "sha512,avx2")]
    pub(super) unsafe fn compress(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
        let [a, b, c, d, e, f, g, h] = state.map(|x| x as i64);
        let mut abef = _mm256_set_epi64x(a, b, e, f);
        let mut cdgh = _mm256_set_epi64x(c, d, g, h);
        for block in blocks {
            let saved = (abef, cdgh);
            let words = schedule(block);
            for i in (0..80).step_by(2) {
                let wk = _mm_set_epi64x(
                    words[i + 1].wrapping_add(K[i + 1]) as i64,
                    words[i].wrapping_add(K[i]) as i64,
                );
                // After two rounds the old ABEF holds the new CDGH.
                (abef, cdgh) = (_mm256_sha512rnds2_epi64(cdgh, abef, wk), abef);
            }
            abef = _mm256_add_epi64(abef, saved.0);
            cdgh = _mm256_add_epi64(cdgh, saved.1);
        }
        // SAFETY: these are the same 32 bytes, with all u64 patterns valid.
        let ([f, e, b, a], [h, g, d, c]) = unsafe {
            (
                std::mem::transmute::<__m256i, [u64; 4]>(abef),
                std::mem::transmute::<__m256i, [u64; 4]>(cdgh),
            )
        };
        *state = [a, b, c, d, e, f, g, h];
    }
}

#[cfg(target_arch = "aarch64")]
mod instructions {
    use super::{K, schedule};
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon,sha3")]
    pub(super) unsafe fn compress(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
        let ptr = state.as_mut_ptr();
        // SAFETY: each load reads two words of the eight-word state.
        let (mut ab, mut cd, mut ef, mut gh) = unsafe {
            (
                vld1q_u64(ptr),
                vld1q_u64(ptr.add(2)),
                vld1q_u64(ptr.add(4)),
                vld1q_u64(ptr.add(6)),
            )
        };
        for block in blocks {
            let saved = (ab, cd, ef, gh);
            let words = schedule(block);
            for i in (0..80).step_by(2) {
                let wk = [
                    words[i + 1].wrapping_add(K[i + 1]),
                    words[i].wrapping_add(K[i]),
                ];
                // SAFETY: wk holds two initialized words.
                let wk = unsafe { vld1q_u64(wk.as_ptr()) };
                // SHA512H returns the two T1 terms in reverse round order,
                // matching the lane order of the new A/B and E/F pairs.
                let t = vsha512hq_u64(
                    vaddq_u64(gh, wk),
                    vextq_u64::<1>(ef, gh),
                    vextq_u64::<1>(cd, ef),
                );
                (ab, cd, ef, gh) = (vsha512h2q_u64(t, cd, ab), ab, vaddq_u64(cd, t), ef);
            }
            ab = vaddq_u64(ab, saved.0);
            cd = vaddq_u64(cd, saved.1);
            ef = vaddq_u64(ef, saved.2);
            gh = vaddq_u64(gh, saved.3);
        }
        // SAFETY: the four stores fill exactly the eight-word state.
        unsafe {
            vst1q_u64(ptr, ab);
            vst1q_u64(ptr.add(2), cd);
            vst1q_u64(ptr.add(4), ef);
            vst1q_u64(ptr.add(6), gh);
        }
    }
}
