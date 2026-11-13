// Reviewed-by: langs-text-921 2026-11-13; float_i32/float_u64 truncate then enforce half-open bounds, rejecting NaN/infinities; LCG conversion matches helper.next_int. Delta review only.
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
        let fields = args
            .map(|x| {
                let (key, value) = x.split_once('=').expect("key=value");
                (key.to_owned(), value.to_owned())
            })
            .collect();
        Self {
            name,
            warmup,
            iterations,
            expected,
            fields,
        }
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
    println!(
        "{} checksum={} expected={} status={}",
        cfg.name,
        checksum,
        cfg.expected,
        if checksum == cfg.expected {
            "OK"
        } else {
            "ERR"
        }
    );
    if checksum != cfg.expected {
        std::process::exit(1);
    }
}

pub struct Lcg {
    state: i32,
}
impl Lcg {
    pub fn new() -> Self {
        Self { state: 42 }
    }
    pub fn reset(&mut self) {
        self.state = 42;
    }
    pub fn integer(&mut self, max: i32) -> i32 {
        self.state = self.state.wrapping_mul(3877).wrapping_add(29573) % 139968;
        float_i32(f64::from(self.state) / 139968.0 * f64::from(max))
    }
    pub fn float(&mut self, max: f64) -> f64 {
        self.state = self.state.wrapping_mul(3877).wrapping_add(29573) % 139968;
        max * f64::from(self.state) / 139968.0
    }
}

pub fn checksum(text: &str) -> u32 {
    text.bytes().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(b))
    })
}

// Mirrors float.to_int followed by result.unwrap, including truncation before range checking.
pub fn float_i32(x: f64) -> i32 {
    let t = x.trunc();
    assert!(t >= -2147483648.0 && t < 2147483648.0, "float.to_int i32");
    t as i32
}

pub fn float_u64(x: f64) -> u64 {
    let t = x.trunc();
    assert!(t >= 0.0 && t < 18446744073709551616.0, "float.to_int u64");
    t as u64
}

#[cfg(test)]
mod tests {
    use super::{float_i32, float_u64};
    #[test]
    fn checked_float_conversions() {
        assert_eq!(float_i32(-1.9), -1);
        assert_eq!(float_i32(2147483647.9), i32::MAX);
        assert_eq!(float_i32(-2147483648.9), i32::MIN);
        assert_eq!(float_u64(-0.9), 0);
        assert_eq!(float_u64(18446744073709549568.0), u64::MAX - 2047);
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 2147483648.0, -2147483649.0] {
            assert!(std::panic::catch_unwind(|| float_i32(x)).is_err());
        }
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 18446744073709551616.0] {
            assert!(std::panic::catch_unwind(|| float_u64(x)).is_err());
        }
    }
}
