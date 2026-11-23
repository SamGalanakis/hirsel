use std::sync::OnceLock;

use super::crc::CASTAGNOLI as POLY;

const fn advance_bit(crc: u32) -> u32 {
    (crc >> 1) ^ (POLY & (crc & 1).wrapping_neg())
}

const fn slicing_table() -> [[u32; 256]; 8] {
    let mut table = [[0; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut row = 0;
        while row < 8 {
            let mut bit = 0;
            while bit < 8 {
                crc = advance_bit(crc);
                bit += 1;
            }
            table[row][byte] = crc;
            row += 1;
        }
        byte += 1;
    }
    table
}

static TABLE: [[u32; 256]; 8] = slicing_table();

fn portable(mut crc: u32, bytes: &[u8]) -> u32 {
    let (words, tail) = bytes.as_chunks::<8>();
    for &word in words {
        let [a, b, c, d, e, f, g, h] = (u64::from_le_bytes(word) ^ u64::from(crc)).to_le_bytes();
        crc = TABLE[7][a as usize]
            ^ TABLE[6][b as usize]
            ^ TABLE[5][c as usize]
            ^ TABLE[4][d as usize]
            ^ TABLE[3][e as usize]
            ^ TABLE[2][f as usize]
            ^ TABLE[1][g as usize]
            ^ TABLE[0][h as usize];
    }
    for &byte in tail {
        crc = TABLE[0][(crc as u8 ^ byte) as usize] ^ (crc >> 8);
    }
    crc
}

type Update = unsafe fn(u32, &[u8]) -> u32;

fn select() -> Update {
    if super::crc::force_portable() {
        return portable;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse4.2") {
        if std::is_x86_feature_detected!("pclmulqdq") {
            return x86::folded;
        }
        return x86::serial;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("crc") {
        return aarch64;
    }
    portable
}

/// Extend a finalized CRC-32C over `len` bytes at `data`.
///
/// # Safety
/// For a nonzero `len`, `data` must point to `len` initialized bytes in one
/// allocation, valid for shared reads for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_crc32c(crc: u32, data: *const u8, len: u64) -> u32 {
    if len == 0 {
        return crc;
    }
    static UPDATE: OnceLock<Update> = OnceLock::new();
    let update = *UPDATE.get_or_init(select);
    // SAFETY: the caller supplies the readable range, and select only returns
    // accelerated functions whose target features the current CPU supports.
    unsafe { !update(!crc, std::slice::from_raw_parts(data, len as usize)) }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::advance_bit;
    use std::arch::x86_64::{
        _mm_clmulepi64_si128, _mm_crc32_u8, _mm_crc32_u64, _mm_cvtsi64_si128, _mm_cvtsi128_si64,
    };

    // Three independent streams hide CRC32's dependency latency. Fold their
    // remainders at the end of each group with carry-less multiplication.
    const STREAM: usize = 256;
    const GROUP: usize = 3 * STREAM;
    const FOLD_A: i64 = fold_factor(2 * STREAM);
    const FOLD_B: i64 = fold_factor(STREAM);

    // Let Z shift a reflected CRC remainder through one zero bit. CRC32 on a
    // word applies Z^64 to that word's bit polynomial. Multiplying by
    // Z^(bytes*8 - 64)(1) before that reduction advances by bytes*8 bits.
    const fn fold_factor(bytes: usize) -> i64 {
        let mut crc = 1;
        let mut bit = 64;
        while bit < bytes * 8 {
            crc = advance_bit(crc);
            bit += 1;
        }
        crc as i64
    }

    #[target_feature(enable = "sse4.2")]
    pub(super) unsafe fn serial(crc: u32, bytes: &[u8]) -> u32 {
        let (words, tail) = bytes.as_chunks::<8>();
        let mut wide = u64::from(crc);
        for &word in words {
            wide = _mm_crc32_u64(wide, u64::from_le_bytes(word));
        }
        let mut crc = wide as u32;
        for &byte in tail {
            crc = _mm_crc32_u8(crc, byte);
        }
        crc
    }

    #[target_feature(enable = "sse4.2,pclmulqdq")]
    pub(super) unsafe fn folded(crc: u32, bytes: &[u8]) -> u32 {
        let factor_a = _mm_cvtsi64_si128(FOLD_A);
        let factor_b = _mm_cvtsi64_si128(FOLD_B);
        let (groups, tail) = bytes.as_chunks::<GROUP>();
        let mut crc = u64::from(crc);
        for group in groups {
            let words = group.as_chunks::<8>().0;
            let mut a = crc;
            let mut b = 0;
            let mut c = 0;
            for i in 0..STREAM / 8 {
                a = _mm_crc32_u64(a, u64::from_le_bytes(words[i]));
                b = _mm_crc32_u64(b, u64::from_le_bytes(words[i + STREAM / 8]));
                c = _mm_crc32_u64(c, u64::from_le_bytes(words[i + 2 * STREAM / 8]));
            }
            let a = _mm_clmulepi64_si128::<0>(_mm_cvtsi64_si128(a as i64), factor_a);
            let b = _mm_clmulepi64_si128::<0>(_mm_cvtsi64_si128(b as i64), factor_b);
            let product = (_mm_cvtsi128_si64(a) ^ _mm_cvtsi128_si64(b)) as u64;
            crc = _mm_crc32_u64(0, product) ^ c;
        }
        // SAFETY: folded also requires SSE4.2.
        unsafe { serial(crc as u32, tail) }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")]
unsafe fn aarch64(mut crc: u32, bytes: &[u8]) -> u32 {
    use std::arch::aarch64::{__crc32cb, __crc32cd};

    let (words, tail) = bytes.as_chunks::<8>();
    for &word in words {
        crc = __crc32cd(crc, u64::from_le_bytes(word));
    }
    for &byte in tail {
        crc = __crc32cb(crc, byte);
    }
    crc
}
