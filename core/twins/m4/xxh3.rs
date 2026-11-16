//! R9 unmodified xxhash-rust comparator, same fixed 4KiB image and loop count.

fn main() {
    let loops: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let image: Vec<u8> = (0u64..4096).map(|i| (i * 17 + 3) as u8).collect();
    let mut sum = 0u64;
    for _ in 0..loops {
        sum = xxhash_rust::xxh3::xxh3_64_with_seed(&image, sum);
    }
    println!("{sum}");
}
