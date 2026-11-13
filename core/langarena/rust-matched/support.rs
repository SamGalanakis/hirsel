use std::collections::BTreeMap;

pub struct Config {
    pub name: String,
    pub warmup: u64,
    pub iterations: u64,
    pub expected: u32,
    fields: BTreeMap<String, String>,
}

impl Config {
    pub fn load() -> Self {
        let mut args = std::env::args().skip(1);
        let name = args.next().expect("task");
        let warmup = args.next().expect("warmup").parse().unwrap();
        let iterations = args.next().expect("iterations").parse().unwrap();
        let expected = args.next().expect("expected").parse().unwrap();
        let fields = args.map(|x| {
            let (key, value) = x.split_once('=').expect("key=value");
            (key.to_owned(), value.to_owned())
        }).collect();
        Self { name, warmup, iterations, expected, fields }
    }
    pub fn number(&self, key: &str) -> usize {
        self.fields[key].parse().unwrap()
    }
    pub fn text(&self, key: &str) -> &str {
        &self.fields[key]
    }
}

pub fn main(run: fn(&Config) -> u32) {
    let cfg = Config::load();
    let checksum = run(&cfg);
    println!("{} checksum={} expected={} status={}", cfg.name, checksum, cfg.expected,
        if checksum == cfg.expected { "OK" } else { "ERR" });
    if checksum != cfg.expected { std::process::exit(1); }
}

pub struct Lcg { state: i32 }
impl Lcg {
    pub fn new() -> Self { Self { state: 42 } }
    pub fn reset(&mut self) { self.state = 42; }
    pub fn integer(&mut self, max: i32) -> i32 {
        self.state = self.state.wrapping_mul(3877).wrapping_add(29573) % 139968;
        (f64::from(self.state) / 139968.0 * f64::from(max)) as i32
    }
    pub fn float(&mut self, max: f64) -> f64 {
        self.state = self.state.wrapping_mul(3877).wrapping_add(29573) % 139968;
        max * f64::from(self.state) / 139968.0
    }
}

pub fn checksum(text: &str) -> u32 {
    text.bytes().fold(5381u32, |h, b| (h << 5).wrapping_add(h).wrapping_add(u32::from(b)))
}
