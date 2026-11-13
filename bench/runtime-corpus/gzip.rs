#![forbid(unsafe_code)]
use std::io::Read;

fn main() {
    let chunk: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    assert!(chunk > 0);
    let repetitions: u64 = std::env::args().nth(2).map_or(1, |s| s.parse().unwrap());
    let (mut total_all, mut checksum_all) = (0u64, 0u64);
    for _ in 0..repetitions {
        let mut decoder = flate2::read::GzDecoder::new(&include_bytes!("gzip.bin")[..]);
        let mut out = vec![0; chunk];
        let (mut total, mut checksum) = (0u64, 0u64);
        loop {
            let n = decoder.read(&mut out).unwrap();
            if n == 0 {
                break;
            }
            total += n as u64;
            checksum += out[..n].iter().map(|&b| b as u64).sum::<u64>();
        }
        assert_eq!((total, checksum), (1048576, 101711872));
        total_all += total;
        checksum_all += checksum;
    }
    println!("{total_all} {checksum_all}");
}
