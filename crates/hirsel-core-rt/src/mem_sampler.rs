//! Geometric gaps between independently sampled bytes.

#[cfg(test)]
const MEAN_BYTES: f64 = (512 * 1024) as f64;
// ln(1 - 1/MEAN_BYTES), evaluated once rather than at every hit.
const LOG_MISS: f64 = -0.000_001_907_350_451_804_216_4;

pub(crate) struct Sampler {
    state: u64,
    until_hit: u64,
}

pub(crate) fn near_miss(peak: u64, limit: u64) -> bool {
    limit != 0 && u128::from(peak) * 5 >= u128::from(limit) * 4
}

impl Sampler {
    pub(crate) fn new(seed: u64) -> Self {
        let mut sampler = Self {
            state: seed,
            until_hit: 0,
        };
        sampler.next_gap();
        sampler
    }

    fn next_gap(&mut self) {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        // Use an open lower endpoint so ln never sees zero.
        let unit = ((mixed >> 11) + 1) as f64 * (1.0 / (1u64 << 53) as f64);
        self.until_hit = (unit.ln() / LOG_MISS).floor() as u64 + 1;
    }

    /// The probability that at least one of this allocation's bytes hits.
    /// A later allocation starts with independent trials even after a hit.
    pub(crate) fn include(&mut self, bytes: u64) -> Option<f64> {
        if bytes < self.until_hit {
            self.until_hit -= bytes;
            return None;
        }
        self.next_gap();
        Some(-(bytes as f64 * LOG_MISS).exp_m1())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_misses_include_eighty_five_percent_and_exclude_seventy_five() {
        assert!(near_miss(85, 100));
        assert!(near_miss(80, 100));
        assert!(!near_miss(75, 100));
        assert!(!near_miss(0, 0));
    }

    #[test]
    fn byte_interval_estimate_respects_inclusion_probability() {
        assert!((LOG_MISS.exp_m1() + 1.0 / MEAN_BYTES).abs() < 1e-18);
        let mut sampler = Sampler::new(12345);
        let mut total = 0u64;
        let mut estimated = 0.0;
        let mut variance = 0.0;
        for bytes in [4_096, 256 * 1024, 2 * 1024 * 1024]
            .into_iter()
            .cycle()
            .take(30_000)
        {
            total += bytes;
            let size = bytes as f64;
            let probability = 1.0 - (1.0 - 1.0 / MEAN_BYTES).powf(size);
            variance += size * size * (1.0 - probability) / probability;
            if let Some(inclusion) = sampler.include(bytes) {
                assert!((inclusion - probability).abs() < 1e-12);
                estimated += size / inclusion;
            }
        }
        // Horvitz-Thompson variance for independent Bernoulli inclusion.
        let error = (estimated - total as f64).abs();
        assert!(
            error <= 6.0 * variance.sqrt(),
            "estimated {estimated}, exact {total}"
        );
        assert_eq!(sampler.include(0), None);
    }
}
