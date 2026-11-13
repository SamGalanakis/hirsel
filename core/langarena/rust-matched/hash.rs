// Review pending: langs-text-921. No matched ratio before independent review.
// Matches hash.hirsel, including upstream SimpleSHA256 (not cryptographic SHA256).
#[allow(dead_code)]
mod support;
use support::Config;
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffffffffu32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb88320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffffffff
}
fn sha256(data: &[u8]) -> u32 {
    let mut hashes = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for (at, b) in data.iter().enumerate() {
        let mut h = hashes[at % 8];
        h = (h << 5).wrapping_add(h).wrapping_add(u32::from(*b));
        hashes[at % 8] = h.wrapping_add(h << 10) ^ (h >> 6);
    }
    let mut digest = [0u8; 32];
    for i in 0..8 {
        let h = hashes[i];
        digest[i * 4] = (h >> 24) as u8;
        digest[i * 4 + 1] = (h >> 16) as u8;
        digest[i * 4 + 2] = (h >> 8) as u8;
        digest[i * 4 + 3] = h as u8;
    }
    u32::from(digest[0])
        | (u32::from(digest[1]) << 8)
        | (u32::from(digest[2]) << 16)
        | (u32::from(digest[3]) << 24)
}
fn run(c: &Config) -> u32 {
    assert!(c.name == "Hash::SHA256" || c.name == "Hash::CRC32");
    let mut data = vec![0u8; c.number("size")];
    let mut g = support::Lcg::new();
    for b in &mut data {
        *b = u8::try_from(g.integer(256)).unwrap();
    }
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        total = total.wrapping_add(if c.name == "Hash::SHA256" {
            sha256(&data)
        } else {
            crc32(&data)
        });
    }
    total
}
fn main() {
    support::main(run);
}
