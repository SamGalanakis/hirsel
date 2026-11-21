//! Shared helpers for the mc-units-proto Rust side.
pub fn pin(cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
    }
}

pub fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// The same xorshift recurrence as the hirsel kernel.
#[inline(never)]
pub fn kernel(seed: u64, rounds: u64) -> u64 {
    let mut x = seed;
    for _ in 0..rounds {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    x
}

// ---- Part 3 session workload: shared definitions (load generator, tool
// server, Tokio twin). The hirsel server (bench/units/session_u.hirsel) speaks
// the same wire format.

pub const REQ: usize = 48;
pub const REPLY: usize = 24;
pub const SVC_ROUNDS: u64 = 1000;
pub const OFFSETS: usize = 4096;
pub const DATA: usize = OFFSETS * 16 + 65536;

pub fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub fn call_size(code: u8) -> usize {
    match code { 0 => 1024, 1 => 8192, _ => 65536 }
}

/// The tool server's reply bytes: a fixed pseudo-random buffer, sliced at
/// (seed & 0xfff) * 16 for `size` bytes.
pub fn tool_data() -> Vec<u8> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..DATA).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect()
}

pub fn offset(seed: u64) -> usize { ((seed & 0xfff) as usize) * 16 }

/// First little-endian word of SHA-256 of each (offset, size code) slice.
pub fn digest_table(data: &[u8]) -> Vec<[u64; 3]> {
    use sha2::{Digest, Sha256};
    (0..OFFSETS).map(|o| {
        let mut row = [0u64; 3];
        for code in 0..3u8 {
            let d = Sha256::digest(&data[o * 16..o * 16 + call_size(code)]);
            row[code as usize] = u64::from_le_bytes(d[..8].try_into().unwrap());
        }
        row
    }).collect()
}

pub fn first_word(b: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(b);
    u64::from_le_bytes(d[..8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug)]
pub struct Turn { pub id: u64, pub seed: u64, pub ncalls: u8, pub par: u8, pub shared: u8, pub codes: [u8; 16] }

impl Turn {
    /// Deterministic from (run seed, turn id). f_permille of turns call the
    /// shared service. Sizes: 1 KiB 50%, 8 KiB 35%, 64 KiB 15%.
    pub fn make(run: u64, id: u64, f_permille: u64, par: bool) -> Turn {
        let h = splitmix(run ^ id.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        let ncalls = if par { 16 } else { 1 + (h >> 8) % 3 } as u8;
        let mut codes = [0u8; 16];
        for (i, c) in codes.iter_mut().enumerate().take(ncalls as usize) {
            let r = splitmix(h.wrapping_add(i as u64 + 1)) % 100;
            *c = if r < 50 { 0 } else if r < 85 { 1 } else { 2 };
        }
        let shared = (splitmix(h ^ 0xABCD) % 1000 < f_permille) as u8;
        Turn { id, seed: h, ncalls, par: par as u8, shared, codes }
    }
    pub fn encode(&self) -> [u8; REQ] {
        let mut b = [0u8; REQ];
        b[0..8].copy_from_slice(&self.id.to_le_bytes());
        b[8..16].copy_from_slice(&self.seed.to_le_bytes());
        b[16] = self.ncalls; b[17] = self.par; b[18] = self.shared;
        b[24..40].copy_from_slice(&self.codes);
        b
    }
    pub fn decode(b: &[u8]) -> Turn {
        let mut codes = [0u8; 16];
        codes.copy_from_slice(&b[24..40]);
        Turn { id: u64::from_le_bytes(b[0..8].try_into().unwrap()), seed: u64::from_le_bytes(b[8..16].try_into().unwrap()),
               ncalls: b[16], par: b[17], shared: b[18], codes }
    }
    /// The checksum a correct server returns, from the digest table.
    pub fn expected(&self, table: &[[u64; 3]]) -> u64 {
        let mut acc = 0u64;
        for i in 0..self.ncalls as u64 {
            let seed = self.seed.wrapping_add(i);
            acc ^= table[(seed & 0xfff) as usize][self.codes[i as usize] as usize];
        }
        if self.shared != 0 { acc ^= kernel(self.id ^ self.seed, SVC_ROUNDS); }
        acc
    }
}
