//! R9 matched byte-table CRC-32C. No hardware acceleration or slicing table.
//! The bit recurrence builds the table at compile time, independently of Hirsel's literals.
// Parity review: std-seed-api-889, followups.md 2026-11-16.
// Carry the CRC between calls so neither compiler can hoist a repeated check.

const fn table() -> [u32; 256] {
    let mut t = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 == 0 { c >> 1 } else { (c >> 1) ^ 0x82f63b78 };
            j += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
const CRC: [u32; 256] = table();

fn crc32c_append(prefix: u32, b: &[u8]) -> u32 {
    let mut c = prefix ^ u32::MAX;
    for &x in b {
        c = CRC[((c ^ u32::from(x)) & 255) as usize] ^ (c >> 8);
    }
    c ^ u32::MAX
}

fn main() {
    let loops: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let image: Vec<u8> = (0u64..4096).map(|i| (i * 17 + 3) as u8).collect();
    let mut c = 0u32;
    for _ in 0..loops {
        c = crc32c_append(c, &image);
    }
    println!("{c}");
}
