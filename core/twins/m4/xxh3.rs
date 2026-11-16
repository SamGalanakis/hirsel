//! R9's unmodified xxhash-rust streaming state; same image, update and digest count.
fn main() {
    let loops: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let image: Vec<u8> = (0u64..4096).map(|i| (i * 17 + 3) as u8).collect();
    let mut state = xxhash_rust::xxh3::Xxh3::new();
    let mut hash = 0u64;
    for _ in 0..loops {
        state.update(&image);
        hash = state.digest();
    }
    println!("{hash}");
}
