//! RustCrypto sha2 0.10.9 oracle and benchmark partner, built with its default
//! backends (SHA-NI when the CPU has it) by core/tools/sha256_reference.py. `vectors` emits the differential corpus;
//! otherwise SIZE LOOPS hashes the same evolving byte image as core/programs/sha256/bench.hirsel.
use sha2::{Digest, Sha256};
use std::hint::black_box;

fn image(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(17).wrapping_add(3)) as u8)
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args[0] == "vectors" {
        let bytes = image(200);
        for n in 0..=200 {
            let expected = Sha256::digest(&bytes[..n]);
            for cut in 0..=n {
                let mut h = Sha256::new();
                h.update(&bytes[..cut]);
                h.update(&bytes[cut..n]);
                assert_eq!(h.finalize(), expected);
            }
            println!("{expected:x}");
        }
        return;
    }
    let size: usize = args[0].parse().unwrap();
    let loops: u64 = args[1].parse().unwrap();
    let mut bytes = image(size);
    let mut digest = [0u8; 32];
    for _ in 0..loops {
        digest = Sha256::digest(black_box(&bytes)).into();
        bytes[0] = digest[0];
    }
    println!("{}", digest[0]);
}
