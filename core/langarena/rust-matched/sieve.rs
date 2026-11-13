// Reviewed-by: langs-text-921 2026-11-13; byte flags, sqrt bound, checked multiples and u64 odd-prime count/last scan match; compile with overflow checks.
// Matches sieve.hirsel: byte flags, sqrt limit, odd count scan, checked indexing.
#[allow(dead_code)]
mod support;
fn run(c: &support::Config) -> u32 {
    assert_eq!(c.name, "Etc::Sieve");
    let n = c.number("limit");
    assert!(n >= 2);
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        let mut primes = vec![1u8; n + 1];
        primes[0] = 0;
        primes[1] = 0;
        let bound = (n as f64).sqrt() as usize;
        for p in 2..bound + 1 {
            if primes[p] == 1 {
                let mut multiple = p * p;
                while multiple <= n {
                    primes[multiple] = 0;
                    multiple += p;
                }
            }
        }
        let mut last = 2u64;
        let mut count = 1u64;
        let mut k = 3;
        while k <= n {
            if primes[k] == 1 {
                last = u64::try_from(k).unwrap();
                count += 1;
            }
            k += 2;
        }
        total = total.wrapping_add((last + count) as u32);
    }
    total
}
fn main() {
    support::main(run);
}
