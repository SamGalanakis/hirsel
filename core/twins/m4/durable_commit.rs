//! Rust twin of the store's durable one-key commit on M4-PLAN §4's matched
//! workload: K=16 keys of 8 bytes, 32-byte values, a log holding 64 earlier
//! commits. Each commit encodes one frame into a reused buffer, appends it with
//! one pwrite, makes it durable with one fdatasync, then publishes the key into
//! the in-memory BTreeMap. The frame is `len u32, seq u64, key [8], value [32],
//! crc32 u32` (little-endian; the CRC covers len to value), 56 bytes.
//!
//! `m4-durable-commit N [check]`: the 64-commit history, then N measured
//! commits. With `check` the run then reopens the log, replays every frame
//! (checking its CRC and its sequence) and compares the replayed map with one
//! computed from the workload alone. It prints the frame count and a checksum
//! of the final in-memory values. The per-commit cost is the difference of
//! runs at N and 0 without `check`, divided by N.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::FileExt;

const KEYS: u64 = 16;
const HISTORY: u64 = 64;
const FRAME: usize = 4 + 8 + 8 + 32 + 4;

type Key = [u8; 8];
type Value = [u8; 32];

/// CRC-32 (IEEE, reflected 0xEDB88320), table built once.
struct Crc([u32; 256]);
impl Crc {
    fn new() -> Self {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        Crc(table)
    }
    fn of(&self, bytes: &[u8]) -> u32 {
        !bytes.iter().fold(!0u32, |c, &b| {
            self.0[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8)
        })
    }
}

struct Store {
    log: File,
    tail: u64,
    seq: u64,
    map: BTreeMap<Key, Value>,
    frame: Vec<u8>,
    crc: Crc,
}

impl Store {
    fn create(path: &std::path::Path) -> std::io::Result<Self> {
        let log = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Store {
            log,
            tail: 0,
            seq: 0,
            map: BTreeMap::new(),
            frame: Vec::with_capacity(FRAME),
            crc: Crc::new(),
        })
    }

    /// One durable one-key commit: encode, pwrite, fdatasync, publish.
    fn commit(&mut self, key: Key, value: Value) -> std::io::Result<()> {
        self.seq += 1;
        let f = &mut self.frame;
        f.clear();
        f.extend_from_slice(&(FRAME as u32).to_le_bytes());
        f.extend_from_slice(&self.seq.to_le_bytes());
        f.extend_from_slice(&key);
        f.extend_from_slice(&value);
        let crc = self.crc.of(f);
        f.extend_from_slice(&crc.to_le_bytes());
        self.log.write_all_at(f, self.tail)?;
        self.log.sync_data()?;
        self.tail += FRAME as u64;
        self.map.insert(key, value);
        Ok(())
    }
}

/// Commit i of the workload: key i mod 16, value 32 bytes derived from i.
fn workload(i: u64) -> (Key, Value) {
    let key = (i % KEYS).to_le_bytes();
    let mut value = [0u8; 32];
    for (j, chunk) in value.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(
            &i.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(j as u32 * 16)
                .to_le_bytes(),
        );
    }
    (key, value)
}

/// Replays the log: every frame whole, its CRC and its sequence in order.
fn replay(path: &std::path::Path) -> (u64, BTreeMap<Key, Value>) {
    let mut bytes = Vec::new();
    File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len() % FRAME, 0, "torn log");
    let crc = Crc::new();
    let mut map = BTreeMap::new();
    let mut count = 0;
    for f in bytes.chunks(FRAME) {
        assert_eq!(
            u32::from_le_bytes(f[0..4].try_into().unwrap()) as usize,
            FRAME
        );
        count += 1;
        assert_eq!(u64::from_le_bytes(f[4..12].try_into().unwrap()), count);
        assert_eq!(
            crc.of(&f[..FRAME - 4]),
            u32::from_le_bytes(f[FRAME - 4..].try_into().unwrap())
        );
        map.insert(f[12..20].try_into().unwrap(), f[20..52].try_into().unwrap());
    }
    (count, map)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: u64 = args
        .first()
        .expect("usage: m4-durable-commit N [check]")
        .parse()
        .unwrap();
    let check = args.get(1).is_some_and(|a| a == "check");
    let dir = std::env::temp_dir().join(format!("m4-durable-commit-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("log");
    let mut store = Store::create(&path).unwrap();
    for i in 0..HISTORY + n {
        let (k, v) = workload(i);
        store.commit(k, v).unwrap();
    }
    let total = HISTORY + n;
    if check {
        let (count, replayed) = replay(&path);
        // The oracle: the last write of each key, from the workload alone.
        let expected: BTreeMap<Key, Value> =
            (total.saturating_sub(KEYS)..total).map(workload).collect();
        assert_eq!(count, total);
        assert_eq!(replayed, expected);
        assert_eq!(store.map, expected);
    }
    std::fs::remove_dir_all(&dir).unwrap();
    let sum = store
        .map
        .values()
        .flatten()
        .fold(0u64, |s, &b| s.wrapping_mul(31).wrapping_add(b as u64));
    println!("{total} {sum}");
}
