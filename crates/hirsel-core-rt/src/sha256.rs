//! `rt.sha256_blocks` (hirsel-core-ir crate docs, "SHA-256"): FIPS 180-4
//! SHA-256 compression of whole 64-byte blocks into an eight-word state.
//!
//! The CPU's SHA-256 instructions run when it has them: the x86-64 SHA
//! extensions, or the ARMv8 SHA-2 instructions. Otherwise the portable code
//! runs. Every path computes the same function, so the choice is never
//! observable (SEM-8). `HIRSEL_SHA256=portable` selects the portable code on
//! any CPU, so the laws can run both paths on one host, as Go's
//! `GODEBUG=cpu.sha=off` does.

use std::sync::OnceLock;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone, Copy)]
enum Path {
    Portable,
    Instructions,
}

fn path() -> Path {
    static PATH: OnceLock<Path> = OnceLock::new();
    *PATH.get_or_init(|| {
        if std::env::var_os("HIRSEL_SHA256").is_some_and(|v| v == "portable") {
            return Path::Portable;
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("sha")
            && std::is_x86_feature_detected!("sse2")
            && std::is_x86_feature_detected!("ssse3")
            && std::is_x86_feature_detected!("sse4.1")
        {
            return Path::Instructions;
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("sha2") {
            return Path::Instructions;
        }
        Path::Portable
    })
}

/// Compress `blocks` 64-byte blocks at `data` into the eight state words.
/// No context, admission or status; zero blocks read nothing.
///
/// # Safety
/// `state` must point to eight writable `u32` words and, when `blocks` is not
/// 0, `data` to `64 × blocks` readable bytes; the two must not overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sha256_blocks(state: *mut u32, data: *const u8, blocks: u64) {
    if blocks == 0 {
        return;
    }
    // SAFETY: the caller passes eight aligned state words and a readable range
    // of 64 × blocks bytes that does not overlap them.
    let (state, blocks) = unsafe {
        (
            &mut *state.cast::<[u32; 8]>(),
            std::slice::from_raw_parts(data.cast::<[u8; 64]>(), blocks as usize),
        )
    };
    compress(state, blocks, path());
}

fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]], path: Path) {
    match path {
        Path::Portable => portable(state, blocks),
        // SAFETY: Instructions is chosen only when the CPU has the features.
        #[cfg(target_arch = "x86_64")]
        Path::Instructions => unsafe { x86::compress(state, blocks) },
        // SAFETY: as above.
        #[cfg(target_arch = "aarch64")]
        Path::Instructions => unsafe { aarch64::compress(state, blocks) },
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        Path::Instructions => portable(state, blocks),
    }
}

/// FIPS 180-4 section 6.2.2.
fn portable(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    for block in blocks {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *x = x.wrapping_add(y);
        }
    }
}

/// Adapted from RustCrypto sha2 0.10.9 src/sha256/x86.rs.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::K;
    use std::arch::x86_64::*;

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    fn schedule(v0: __m128i, v1: __m128i, v2: __m128i, v3: __m128i) -> __m128i {
        let t1 = _mm_sha256msg1_epu32(v0, v1);
        let t2 = _mm_alignr_epi8(v3, v2, 4);
        _mm_sha256msg2_epu32(_mm_add_epi32(t1, t2), v3)
    }

    /// Four rounds from the scheduled words `w` and constants `K[4i..4i + 4]`.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    fn rounds4(abef: &mut __m128i, cdgh: &mut __m128i, w: __m128i, i: usize) {
        let k = _mm_set_epi32(
            K[4 * i + 3] as i32,
            K[4 * i + 2] as i32,
            K[4 * i + 1] as i32,
            K[4 * i] as i32,
        );
        let t1 = _mm_add_epi32(w, k);
        *cdgh = _mm_sha256rnds2_epu32(*cdgh, *abef, t1);
        let t2 = _mm_shuffle_epi32(t1, 0x0E);
        *abef = _mm_sha256rnds2_epu32(*abef, *cdgh, t2);
    }

    /// # Safety
    /// The CPU must have the SHA, SSE2, SSSE3 and SSE4.1 features.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub(super) unsafe fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        let mask = _mm_set_epi64x(
            0x0C0D_0E0F_0809_0A0Bu64 as i64,
            0x0405_0607_0001_0203u64 as i64,
        );
        let at = state.as_ptr().cast::<__m128i>();
        // SAFETY: the state is 32 readable bytes; the loads are unaligned.
        let (dcba, efgh) = unsafe { (_mm_loadu_si128(at), _mm_loadu_si128(at.add(1))) };
        let cdab = _mm_shuffle_epi32(dcba, 0xB1);
        let efgh = _mm_shuffle_epi32(efgh, 0x1B);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xF0);
        for block in blocks {
            let (abef_save, cdgh_save) = (abef, cdgh);
            let at = block.as_ptr().cast::<__m128i>();
            // SAFETY: each block is 64 readable bytes; the loads are unaligned.
            let load = |i| _mm_shuffle_epi8(unsafe { _mm_loadu_si128(at.add(i)) }, mask);
            // The last four scheduled words rotate through locals, not an
            // indexed array, so they stay in registers.
            let (mut w0, mut w1, mut w2, mut w3) = (load(0), load(1), load(2), load(3));
            for (i, w) in [w0, w1, w2, w3].into_iter().enumerate() {
                rounds4(&mut abef, &mut cdgh, w, i);
            }
            for i in 4..16 {
                (w0, w1, w2, w3) = (w1, w2, w3, schedule(w0, w1, w2, w3));
                rounds4(&mut abef, &mut cdgh, w3, i);
            }
            abef = _mm_add_epi32(abef, abef_save);
            cdgh = _mm_add_epi32(cdgh, cdgh_save);
        }
        let feba = _mm_shuffle_epi32(abef, 0x1B);
        let dchg = _mm_shuffle_epi32(cdgh, 0xB1);
        let dcba = _mm_blend_epi16(feba, dchg, 0xF0);
        let hgef = _mm_alignr_epi8(dchg, feba, 8);
        let at = state.as_mut_ptr().cast::<__m128i>();
        // SAFETY: the state is 32 writable bytes; the stores are unaligned.
        unsafe {
            _mm_storeu_si128(at, dcba);
            _mm_storeu_si128(at.add(1), hgef);
        }
    }
}

/// Adapted from RustCrypto sha2 0.10.9 src/sha256/aarch64.rs, with the
/// standard library's intrinsics in place of its inline assembly.
#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use super::K;
    use std::arch::aarch64::*;

    /// # Safety
    /// The CPU must have the SHA-2 feature.
    #[target_feature(enable = "sha2")]
    pub(super) unsafe fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        // SAFETY: the state is eight readable words, K sixty-four, and each
        // block 64 readable bytes.
        unsafe {
            let mut abcd = vld1q_u32(state.as_ptr());
            let mut efgh = vld1q_u32(state.as_ptr().add(4));
            for block in blocks {
                let (abcd_save, efgh_save) = (abcd, efgh);
                let load = |i: usize| {
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(16 * i))))
                };
                // The last four scheduled words rotate through locals, not an
                // indexed array, so they stay in registers.
                let (mut s0, mut s1, mut s2, mut s3) = (load(0), load(1), load(2), load(3));
                for i in 0..16 {
                    let w = match i {
                        0 => s0,
                        1 => s1,
                        2 => s2,
                        3 => s3,
                        _ => {
                            let next = vsha256su1q_u32(vsha256su0q_u32(s0, s1), s2, s3);
                            (s0, s1, s2, s3) = (s1, s2, s3, next);
                            next
                        }
                    };
                    let wk = vaddq_u32(w, vld1q_u32(K.as_ptr().add(4 * i)));
                    let abcd_prev = abcd;
                    abcd = vsha256hq_u32(abcd_prev, efgh, wk);
                    efgh = vsha256h2q_u32(efgh, abcd_prev, wk);
                }
                abcd = vaddq_u32(abcd, abcd_save);
                efgh = vaddq_u32(efgh, efgh_save);
            }
            vst1q_u32(state.as_mut_ptr(), abcd);
            vst1q_u32(state.as_mut_ptr().add(4), efgh);
        }
    }
}
