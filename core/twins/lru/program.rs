use crate::support::*;
pub trait Cache: Default {
    fn put(&mut self, key: &str, value: &str) -> bool;
    fn get(&mut self, key: &str) -> Option<Vec<u8>>;
    fn order(&self) -> Vec<String>;
    fn first_is_stale(&self) -> bool;
}
pub fn run<C: Cache>() {
    let (input, variant) = input();
    let trace = Trace::default();
    let admission = Admission::new(&variant);
    let mut cache = C::default();
    let mut hits = 0;
    let mut checksum = 0;
    let mut evictions = 0;
    let mut puts = 0;
    let owner = Resource::new("cache", &trace);
    let result = (|| {
        for line in input.lines() {
            let op: Vec<_> = line.split_whitespace().collect();
            match op[0] {
                "P" => {
                    puts += 1;
                    admission.at(&format!("put{puts}.payload"))?;
                    evictions += usize::from(cache.put(op[1], op[2]));
                }
                "G" => {
                    if let Some(bytes) = cache.get(op[1]) {
                        hits += 1;
                        checksum += sum(&bytes);
                    }
                }
                "S" => {
                    if cache.first_is_stale() {
                        return Err("StaleHandle@first-entry".into());
                    }
                }
                _ => return Err("Parse@cache-op".into()),
            }
        }
        Ok(())
    })();
    println!("hits={hits} sum={checksum} evictions={evictions}");
    println!("order={}", cache.order().join(","));
    drop(cache);
    drop(owner);
    trace.print();
    finish(result);
}
