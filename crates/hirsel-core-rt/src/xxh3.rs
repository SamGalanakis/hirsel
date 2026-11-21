//! The seed-zero XXH3 short hash and the portable and vector stripe updates.

type Accumulate = unsafe fn(&mut [u64; 8], &[[u8; 64]], &[u8]);

fn select() -> Accumulate {
    if std::env::var_os("HIRSEL_XXH3").is_some_and(|v| v == "portable") {
        return portable;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        return avx2::accumulate;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return neon::accumulate;
    }
    portable
}

/// Hash at most 240 bytes with seed zero and the default secret.
///
/// # Safety
/// When `len` is nonzero, `data` points to `len` readable bytes. The caller
/// checks the 240-byte limit before entering this row.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_xxh3_short(data: *const u8, len: u64) -> u64 {
    let input = if len == 0 {
        &[]
    } else {
        // SAFETY: the caller provides the complete byte span.
        unsafe { std::slice::from_raw_parts(data, len as usize) }
    };
    twox_hash::XxHash3_64::oneshot(input)
}

/// Accumulate `count` XXH3 stripes, advancing eight bytes in the secret
/// per stripe. The caller handles block scrambling and the final stripe.
///
/// # Safety
/// `acc` is eight aligned writable words. When `count` is nonzero, `data`
/// contains `64 * count` readable bytes and `secret` contains at least
/// `64 + 8 * (count - 1)` readable bytes. Neither overlaps `acc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_xxh3_stripes(
    acc: *mut u64,
    data: *const u8,
    secret: *const u8,
    count: u64,
) {
    if count == 0 {
        return;
    }
    static ACCUMULATE: std::sync::OnceLock<Accumulate> = std::sync::OnceLock::new();
    let count = count as usize;
    // SAFETY: the front end checked both source ranges, and select only
    // chooses a kernel when its target features are present.
    unsafe {
        let acc = &mut *acc.cast::<[u64; 8]>();
        let data = std::slice::from_raw_parts(data.cast::<[u8; 64]>(), count);
        let secret = std::slice::from_raw_parts(secret, 64 + 8 * (count - 1));
        ACCUMULATE.get_or_init(select)(acc, data, secret);
    }
}

fn portable(acc: &mut [u64; 8], stripes: &[[u8; 64]], secret: &[u8]) {
    for (stripe, secret) in stripes.iter().zip(secret.windows(64).step_by(8)) {
        for (i, (data, key)) in stripe
            .as_chunks::<8>()
            .0
            .iter()
            .zip(secret.as_chunks::<8>().0)
            .enumerate()
        {
            let data = u64::from_le_bytes(*data);
            let keyed = data ^ u64::from_le_bytes(*key);
            acc[i ^ 1] = acc[i ^ 1].wrapping_add(data);
            acc[i] = acc[i].wrapping_add((keyed & 0xffff_ffff) * (keyed >> 32));
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    fn stripe(acc: __m256i, data: __m256i, secret: __m256i) -> __m256i {
        let keyed = _mm256_xor_si256(data, secret);
        let product = _mm256_mul_epu32(keyed, _mm256_srli_epi64::<32>(keyed));
        let swapped = _mm256_shuffle_epi32::<0b01_00_11_10>(data);
        _mm256_add_epi64(_mm256_add_epi64(acc, swapped), product)
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn accumulate(acc: &mut [u64; 8], stripes: &[[u8; 64]], secret: &[u8]) {
        let acc = acc.as_mut_ptr().cast::<__m256i>();
        // SAFETY: all loads and stores are unaligned. The caller supplies a
        // 64-byte secret window at each eight-byte step.
        unsafe {
            let mut low = _mm256_loadu_si256(acc);
            let mut high = _mm256_loadu_si256(acc.add(1));
            for (i, block) in stripes.iter().enumerate() {
                let data = block.as_ptr().cast::<__m256i>();
                let key = secret.as_ptr().add(i * 8).cast::<__m256i>();
                low = stripe(low, _mm256_loadu_si256(data), _mm256_loadu_si256(key));
                high = stripe(
                    high,
                    _mm256_loadu_si256(data.add(1)),
                    _mm256_loadu_si256(key.add(1)),
                );
            }
            _mm256_storeu_si256(acc, low);
            _mm256_storeu_si256(acc.add(1), high);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    fn stripe(acc: uint64x2_t, data: uint64x2_t, secret: uint64x2_t) -> uint64x2_t {
        let keyed = veorq_u64(data, secret);
        let low = vmovn_u64(keyed);
        let high = vshrn_n_u64::<32>(keyed);
        let with_data = vaddq_u64(acc, vextq_u64::<1>(data, data));
        vmlal_u32(with_data, low, high)
    }

    #[target_feature(enable = "neon")]
    pub(super) unsafe fn accumulate(acc: &mut [u64; 8], stripes: &[[u8; 64]], secret: &[u8]) {
        let acc = acc.as_mut_ptr();
        // SAFETY: NEON loads accept unaligned addresses. Every window and
        // stripe is 64 readable bytes, and acc has eight writable words.
        unsafe {
            let mut a0 = vld1q_u64(acc);
            let mut a1 = vld1q_u64(acc.add(2));
            let mut a2 = vld1q_u64(acc.add(4));
            let mut a3 = vld1q_u64(acc.add(6));
            for (i, block) in stripes.iter().enumerate() {
                let data = block.as_ptr().cast::<u64>();
                let key = secret.as_ptr().add(i * 8).cast::<u64>();
                a0 = stripe(a0, vld1q_u64(data), vld1q_u64(key));
                a1 = stripe(a1, vld1q_u64(data.add(2)), vld1q_u64(key.add(2)));
                a2 = stripe(a2, vld1q_u64(data.add(4)), vld1q_u64(key.add(4)));
                a3 = stripe(a3, vld1q_u64(data.add(6)), vld1q_u64(key.add(6)));
            }
            vst1q_u64(acc, a0);
            vst1q_u64(acc.add(2), a1);
            vst1q_u64(acc.add(4), a2);
            vst1q_u64(acc.add(6), a3);
        }
    }
}
