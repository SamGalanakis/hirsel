mod program;
#[path = "../support.rs"]
mod support;
use std::collections::HashMap;
#[derive(Default)]
struct Counts {
    index: HashMap<String, usize>,
    ordered: Vec<(String, u64)>,
}
impl program::OrderedCounts for Counts {
    fn add(&mut self, key: &str) {
        if let Some(&i) = self.index.get(key) {
            self.ordered[i].1 += 1;
        } else {
            self.index.insert(key.into(), self.ordered.len());
            self.ordered.push((key.into(), 1));
        }
    }
    fn entries(&self) -> Vec<(&str, u64)> {
        self.ordered.iter().map(|(k, v)| (k.as_str(), *v)).collect()
    }
}
fn main() {
    program::run::<Counts>();
}
