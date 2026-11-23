//! Finalized CRC-64/NVME updates and concatenation for both CRC widths.

use std::sync::OnceLock;

const NVME: u64 = 0x9a6c9329ac4bc9b5;
pub(super) const CASTAGNOLI: u32 = 0x82f63b78;

pub(super) fn force_portable() -> bool {
    std::env::var_os("HIRSEL_CRC").is_some_and(|value| value == "portable")
}

const fn zero_bit(crc: u64, poly: u64) -> u64 {
    (crc >> 1) ^ (poly & (crc & 1).wrapping_neg())
}

const fn slicing_table() -> [[u64; 256]; 8] {
    let mut table = [[0; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u64;
        let mut row = 0;
        while row < 8 {
            let mut bit = 0;
            while bit < 8 {
                crc = zero_bit(crc, NVME);
                bit += 1;
            }
            table[row][byte] = crc;
            row += 1;
        }
        byte += 1;
    }
    table
}

static TABLE: [[u64; 256]; 8] = slicing_table();

fn portable(mut crc: u64, bytes: &[u8]) -> u64 {
    let (words, tail) = bytes.as_chunks::<8>();
    for &word in words {
        let [a, b, c, d, e, f, g, h] = (u64::from_le_bytes(word) ^ crc).to_le_bytes();
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

type Update = unsafe fn(u64, &[u8]) -> u64;

fn select() -> Update {
    if force_portable() {
        return portable;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("pclmulqdq") {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("vpclmulqdq") {
            return x86::wide;
        }
        return x86::folded;
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    if std::arch::is_aarch64_feature_detected!("pmull")
        && std::arch::is_aarch64_feature_detected!("aes")
    {
        return arm::folded;
    }
    portable
}

/// Extend a finalized CRC-64/NVME.
///
/// # Safety
/// For nonzero `len`, `data` must point to `len` initialized bytes in one
/// allocation, valid for shared reads for this call, with `len <= isize::MAX`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_crc64nvme(crc: u64, data: *const u8, len: u64) -> u64 {
    if len == 0 {
        return crc;
    }
    static UPDATE: OnceLock<Update> = OnceLock::new();
    let update = *UPDATE.get_or_init(select);
    // SAFETY: the caller provides the readable range and select checks CPU features.
    unsafe { !update(!crc, std::slice::from_raw_parts(data, len as usize)) }
}

// In reflected order the top bit is the constant coefficient (one).
// Multiplication by x is the same linear map as processing one zero bit.
const fn multiply(mut value: u64, mut factor: u64, poly: u64, one: u64) -> u64 {
    let mut product = 0;
    while factor != 0 {
        if factor & one != 0 {
            product ^= value;
        }
        factor = (factor << 1) & (one | (one - 1));
        value = zero_bit(value, poly);
    }
    product
}

const fn byte_powers(poly: u64, one: u64) -> [u64; 64] {
    let mut power = one >> 8;
    let mut result = [0; 64];
    let mut i = 0;
    while i < result.len() {
        result[i] = power;
        power = multiply(power, power, poly, one);
        i += 1;
    }
    result
}

fn combine(mut a: u64, b: u64, mut len_b: u64, powers: &[u64; 64], poly: u64, one: u64) -> u64 {
    // Initial and final complements cancel because b starts with the standard seed.
    while len_b != 0 {
        let bit = len_b.trailing_zeros();
        a = multiply(a, powers[bit as usize], poly, one);
        len_b &= len_b - 1;
    }
    a ^ b
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_crc32c_combine(a: u32, b: u32, len_b: u64) -> u32 {
    const POLY: u64 = CASTAGNOLI as u64;
    const ONE: u64 = 1 << 31;
    static POWERS: [u64; 64] = byte_powers(POLY, ONE);
    combine(a.into(), b.into(), len_b, &POWERS, POLY, ONE) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_crc64nvme_combine(a: u64, b: u64, len_b: u64) -> u64 {
    const ONE: u64 = 1 << 63;
    static POWERS: [u64; 64] = byte_powers(NVME, ONE);
    combine(a, b, len_b, &POWERS, NVME, ONE)
}

#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
))]
#[path = "crc/folding.rs"]
mod folding;

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
use folding::arm;
#[cfg(target_arch = "x86_64")]
use folding::x86;
