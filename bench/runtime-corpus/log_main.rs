//! Standalone adapter for the existing LangArena LogParser reference.
#![forbid(unsafe_code)]
mod log_parser;

pub trait Benchmark {
    fn name(&self) -> String;
    fn prepare(&mut self);
    fn run(&mut self, iteration: i64);
    fn checksum(&self) -> u32;
}

pub fn config_i64(_group: &str, key: &str) -> i64 {
    assert_eq!(key, "lines_count");
    std::env::args().nth(1).unwrap().parse().unwrap()
}

fn main() {
    let iterations: i64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let mut bench = log_parser::LogParser::new();
    bench.prepare();
    for i in 0..iterations {
        bench.run(i);
    }
    println!("{}", bench.checksum());
}
