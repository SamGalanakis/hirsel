//! RFC 3174 SHA-1 over a span of complete blocks and its padding.
//! The SHA instruction rounds adapt RustCrypto sha1 0.11.0.

type Compress = unsafe fn(&mut [u32; 5], &[[u8; 64]]);

fn select() -> Compress {
    if std::env::var_os("HIRSEL_SHA1").is_some_and(|v| v == "portable") {
        return portable;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sha")
        && std::is_x86_feature_detected!("ssse3")
        && std::is_x86_feature_detected!("sse4.1")
    {
        return x86::compress;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        return aarch64::compress;
    }
    portable
}

fn compressor() -> Compress {
    static COMPRESS: std::sync::OnceLock<Compress> = std::sync::OnceLock::new();
    *COMPRESS.get_or_init(select)
}

/// Compress complete RFC 3174 blocks without padding.
///
/// # Safety
/// `state` points to five aligned writable words. When `blocks` is nonzero,
/// `data` points to that many 64-byte blocks disjoint from `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sha1_compress(state: *mut u32, data: *const u8, blocks: u64) {
    if blocks == 0 {
        return;
    }
    // SAFETY: both spans meet the caller preconditions. The selected kernel
    // only uses CPU features that are present.
    unsafe {
        let state = &mut *state.cast::<[u32; 5]>();
        let blocks = std::slice::from_raw_parts(data.cast::<[u8; 64]>(), blocks as usize);
        compressor()(state, blocks);
    }
}

/// Compress a byte span and its RFC 3174 padding.
///
/// # Safety
/// `state` points to five aligned writable words. When `len` is nonzero,
/// `data` points to `len` readable bytes disjoint from `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sha1_blocks(state: *mut u32, data: *const u8, len: u64) {
    // SAFETY: both spans meet the caller preconditions. Empty Bytes may
    // have a null data pointer.
    let state = unsafe { &mut *state.cast::<[u32; 5]>() };
    let input = if len == 0 {
        &[]
    } else {
        // SAFETY: nonempty span is readable and disjoint from the state.
        unsafe { std::slice::from_raw_parts(data, len as usize) }
    };
    let (blocks, tail) = input.as_chunks::<64>();
    let compress = compressor();

    // SAFETY: select only chooses a kernel when its CPU features are present.
    if !blocks.is_empty() {
        unsafe { compress(state, blocks) };
    }
    let mut padded = [0; 128];
    padded[..tail.len()].copy_from_slice(tail);
    padded[tail.len()] = 0x80;
    let end = if tail.len() < 56 { 64 } else { 128 };
    padded[end - 8..end].copy_from_slice(&(len << 3).to_be_bytes());
    // SAFETY: same selected kernel, with its CPU features present.
    unsafe { compress(state, padded[..end].as_chunks::<64>().0) };
}

const K: [u32; 4] = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6];

fn portable(state: &mut [u32; 5], blocks: &[[u8; 64]]) {
    for block in blocks {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *state;
        for (i, word) in w.into_iter().enumerate() {
            let f = match i / 20 {
                0 => (b & c) | (!b & d),
                2 => (b & c) | (b & d) | (c & d),
                _ => b ^ c ^ d,
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(K[i / 20])
                .wrapping_add(word);
            (a, b, c, d, e) = (next, a, b.rotate_left(30), c, d);
        }
        for (word, update) in state.iter_mut().zip([a, b, c, d, e]) {
            *word = word.wrapping_add(update);
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "sha,ssse3,sse4.1")]
    pub(super) unsafe fn compress(state: &mut [u32; 5], blocks: &[[u8; 64]]) {
        let reverse = _mm_set_epi64x(0x0001_0203_0405_0607, 0x0809_0a0b_0c0d_0e0f);
        // SAFETY: four words are readable at state, and the load is unaligned.
        let mut abcd = _mm_shuffle_epi32::<0x1b>(unsafe { _mm_loadu_si128(state.as_ptr().cast()) });
        let mut e = _mm_set_epi32(state[4] as i32, 0, 0, 0);

        for block in blocks {
            // SAFETY: i is 0 to 3, so each load reads 16 bytes inside the block.
            let load = |i: usize| unsafe {
                _mm_shuffle_epi8(
                    _mm_loadu_si128(block.as_ptr().cast::<__m128i>().add(i)),
                    reverse,
                )
            };
            let (mut w0, mut w1, mut w2, mut w3) = (load(0), load(1), load(2), load(3));
            let mut prev = abcd;
            let mut now = _mm_sha1rnds4_epu32::<0>(abcd, _mm_add_epi32(e, w0));

            // SHA1NEXTE derives the fifth state word from the previous ABCD.
            // Keep the four schedule vectors in locals to hold them in registers.
            macro_rules! round {
                ($group:literal, $word:expr) => {{
                    let next = _mm_sha1rnds4_epu32::<$group>(now, _mm_sha1nexte_epu32(prev, $word));
                    (prev, now) = (now, next);
                }};
            }
            macro_rules! scheduled {
                ($group:literal) => {{
                    let first = _mm_xor_si128(_mm_sha1msg1_epu32(w0, w1), w2);
                    let next = _mm_sha1msg2_epu32(first, w3);
                    (w0, w1, w2, w3) = (w1, w2, w3, next);
                    round!($group, next);
                }};
            }
            macro_rules! group {
                ($g:literal) => {
                    scheduled!($g);
                    scheduled!($g);
                    scheduled!($g);
                    scheduled!($g);
                    scheduled!($g);
                };
            }

            round!(0, w1);
            round!(0, w2);
            round!(0, w3);
            scheduled!(0);
            group!(1);
            group!(2);
            scheduled!(3);
            scheduled!(3);
            scheduled!(3);
            scheduled!(3);
            let first = _mm_xor_si128(_mm_sha1msg1_epu32(w0, w1), w2);
            round!(3, _mm_sha1msg2_epu32(first, w3));

            abcd = _mm_add_epi32(abcd, now);
            e = _mm_sha1nexte_epu32(prev, e);
        }

        // SAFETY: state has four writable words at its base.
        unsafe { _mm_storeu_si128(state.as_mut_ptr().cast(), _mm_shuffle_epi32::<0x1b>(abcd)) };
        state[4] = _mm_extract_epi32::<3>(e) as u32;
    }
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use super::K;
    use std::arch::aarch64::*;

    #[target_feature(enable = "sha2")]
    pub(super) unsafe fn compress(state: &mut [u32; 5], blocks: &[[u8; 64]]) {
        // SAFETY: four words are readable at state.
        let mut abcd = unsafe { vld1q_u32(state.as_ptr()) };
        let mut e = state[4];
        let [k0, k1, k2, k3] = K.map(|k| vdupq_n_u32(k));
        for block in blocks {
            // SAFETY: i is 0 to 3, so each load reads 16 bytes inside the block.
            let load = |i: usize| unsafe {
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(16 * i))))
            };
            let (mut w0, mut w1, mut w2, mut w3) = (load(0), load(1), load(2), load(3));
            let (mut now, mut next_e) = (abcd, e);
            macro_rules! round {
                ($op:ident, $k:ident, $word:expr) => {{
                    let fifth = vsha1h_u32(vgetq_lane_u32::<0>(now));
                    now = $op(now, next_e, vaddq_u32($word, $k));
                    next_e = fifth;
                }};
            }
            macro_rules! scheduled {
                ($op:ident, $k:ident) => {{
                    let next = vsha1su1q_u32(vsha1su0q_u32(w0, w1, w2), w3);
                    (w0, w1, w2, w3) = (w1, w2, w3, next);
                    round!($op, $k, w3);
                }};
            }
            macro_rules! group {
                ($op:ident, $k:ident) => {
                    scheduled!($op, $k);
                    scheduled!($op, $k);
                    scheduled!($op, $k);
                    scheduled!($op, $k);
                    scheduled!($op, $k);
                };
            }

            round!(vsha1cq_u32, k0, w0);
            round!(vsha1cq_u32, k0, w1);
            round!(vsha1cq_u32, k0, w2);
            round!(vsha1cq_u32, k0, w3);
            scheduled!(vsha1cq_u32, k0);
            group!(vsha1pq_u32, k1);
            group!(vsha1mq_u32, k2);
            scheduled!(vsha1pq_u32, k3);
            scheduled!(vsha1pq_u32, k3);
            scheduled!(vsha1pq_u32, k3);
            scheduled!(vsha1pq_u32, k3);
            let last = vsha1su1q_u32(vsha1su0q_u32(w0, w1, w2), w3);
            round!(vsha1pq_u32, k3, last);

            abcd = vaddq_u32(abcd, now);
            e = e.wrapping_add(next_e);
        }
        // SAFETY: state has four writable words at its base.
        unsafe { vst1q_u32(state.as_mut_ptr(), abcd) };
        state[4] = e;
    }
}
