//! Matched M1 source helpers; idiomatic twins retain support.rs unchanged.
#![allow(dead_code)]
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::mpsc::{self, Receiver, SyncSender};
pub type Result<T> = std::result::Result<T, String>;
pub fn input() -> Result<(String, String)> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("Parse@arguments")?;
    let text = std::fs::read_to_string(path).map_err(|_| "Io@input")?;
    Ok((text, args.next().unwrap_or_else(|| "normal".into())))
}
pub fn line<'a>(s: &'a str, at: &mut usize) -> &'a str {
    let lo = *at;
    let hi = s[lo..].find('\n').map_or(s.len(), |i| lo + i);
    *at = if hi < s.len() { hi + 1 } else { hi };
    let end = if hi > lo && s.as_bytes()[hi - 1] == 13 {
        hi - 1
    } else {
        hi
    };
    &s[lo..end]
}
pub fn number(s: &str) -> Result<u64> {
    if s.is_empty() {
        return Err("Parse@number".into());
    }
    let mut value = 0u64;
    let mut big = false;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return Err("Parse@number".into());
        }
        let d = u64::from(b - b'0');
        if value <= 1_844_674_407_370_955_160 {
            value = value.wrapping_mul(10).wrapping_add(d);
        } else if !big {
            match value.checked_mul(10).and_then(|n| n.checked_add(d)) {
                Some(n) => value = n,
                None => big = true,
            }
        }
    }
    if big {
        Err("Parse@number".into())
    } else {
        Ok(value)
    }
}
pub fn index(n: u64) -> usize {
    usize::try_from(n).unwrap()
}
pub fn narrow(n: usize) -> u32 {
    u32::try_from(n).unwrap()
}
pub fn sum(xs: &[u8]) -> u64 {
    if xs.len() < 72_057_594_037_927_936 {
        let (mut a, mut c, mut d, mut e) = (0u64, 0u64, 0u64, 0u64);
        let mut i = 0;
        while xs.len() - i >= 4 {
            a = a.wrapping_add(u64::from(xs[i]));
            c = c.wrapping_add(u64::from(xs[i + 1]));
            d = d.wrapping_add(u64::from(xs[i + 2]));
            e = e.wrapping_add(u64::from(xs[i + 3]));
            i += 4;
        }
        let mut total = a.wrapping_add(c).wrapping_add(d.wrapping_add(e));
        while i < xs.len() {
            total = total.wrapping_add(u64::from(xs[i]));
            i += 1;
        }
        total
    } else {
        xs.iter().fold(0u64, |a, b| a + u64::from(*b))
    }
}
pub fn num(out: &mut String, n: impl std::fmt::Display) {
    write!(out, "{n}").unwrap();
}
pub fn name(a: &str, n: impl std::fmt::Display, b: &str) -> String {
    let mut s = a.to_owned();
    num(&mut s, n);
    s.push_str(b);
    s
}
pub fn admission(variant: &str, site: &str) -> Result<()> {
    if variant.strip_prefix("capacity:") == Some(site) {
        Err(format!("Capacity@{site}"))
    } else {
        Ok(())
    }
}
pub fn put(s: &str) -> Result<()> {
    std::io::stdout()
        .write_all(s.as_bytes())
        .map_err(|_| "Io@stdout".into())
}
pub fn exit(result: Result<()>) {
    if let Err(e) = result {
        let _ = put("failure=");
        let _ = put(&e);
        let _ = put("\n");
        std::process::exit(if e.starts_with("Capacity") {
            3
        } else if e.starts_with("Cancelled") {
            4
        } else {
            2
        });
    }
}
pub struct Trace {
    tx: SyncSender<String>,
    rx: Receiver<String>,
}
pub struct Marker {
    release: String,
    tx: SyncSender<String>,
}
impl Trace {
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel(capacity);
        Self { tx, rx }
    }
    pub fn marker(&self, label: &str) -> Marker {
        let mut acquire = "acquire=".to_owned();
        acquire.push_str(label);
        let mut release = "release=".to_owned();
        release.push_str(label);
        let marker = Marker {
            release,
            tx: self.tx.clone(),
        };
        self.tx.try_send(acquire).unwrap();
        marker
    }
    pub fn finish(self, result: Result<()>) {
        drop(self.tx);
        for event in self.rx {
            exit(put(&event).and_then(|_| put("\n")));
        }
        exit(result);
    }
}
impl Drop for Marker {
    fn drop(&mut self) {
        let _ = self.tx.try_send(self.release.clone());
    }
}
