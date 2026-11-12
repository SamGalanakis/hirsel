mod program;
#[path = "../support.rs"]
mod support;
#[derive(Default)]
struct Counts {
    bytes: Vec<u8>,
    entries: Vec<(u32, u32, u64)>,
    buckets: Vec<Option<usize>>,
}
impl Counts {
    fn hash(key: &str) -> usize {
        key.bytes()
            .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
    }
    fn grow(&mut self) {
        self.buckets = vec![None; (self.buckets.len() * 2).max(16)];
        for (i, &(a, b, _)) in self.entries.iter().enumerate() {
            let mut h =
                Self::hash(std::str::from_utf8(&self.bytes[a as usize..b as usize]).unwrap())
                    % self.buckets.len();
            while self.buckets[h].is_some() {
                h = (h + 1) % self.buckets.len();
            }
            self.buckets[h] = Some(i);
        }
    }
}
impl program::OrderedCounts for Counts {
    fn add(&mut self, key: &str) {
        if (self.entries.len() + 1) * 2 >= self.buckets.len() {
            self.grow();
        }
        let mut h = Self::hash(key) % self.buckets.len();
        while let Some(i) = self.buckets[h] {
            let (a, b, _) = self.entries[i];
            if &self.bytes[a as usize..b as usize] == key.as_bytes() {
                self.entries[i].2 += 1;
                return;
            }
            h = (h + 1) % self.buckets.len();
        }
        let a = u32::try_from(self.bytes.len()).unwrap();
        self.bytes.extend_from_slice(key.as_bytes());
        self.buckets[h] = Some(self.entries.len());
        self.entries
            .push((a, u32::try_from(self.bytes.len()).unwrap(), 1));
    }
    fn entries(&self) -> Vec<(&str, u64)> {
        self.entries
            .iter()
            .map(|&(a, b, n)| {
                (
                    std::str::from_utf8(&self.bytes[a as usize..b as usize]).unwrap(),
                    n,
                )
            })
            .collect()
    }
}
fn main() {
    program::run::<Counts>();
}
