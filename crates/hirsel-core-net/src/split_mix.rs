//! Deterministic draws for explicit schedules.

pub struct SplitMix64(u64);
impl SplitMix64 {
    pub fn new(seed: u64, stream: u64) -> Self {
        Self(Self::mix(seed ^ stream.wrapping_mul(0x9e3779b97f4a7c15)))
    }

    pub fn choose(&mut self, alternatives: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        (Self::mix(self.0) % alternatives as u64) as usize
    }

    fn mix(mut value: u64) -> u64 {
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
}
