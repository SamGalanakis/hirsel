//! Source-matched M1 helpers. Library routines port core/std bodies, not std algorithms.
#![allow(dead_code)]
mod matched_storage;
pub use matched_storage::{List as Vec, Text as String, View, encode_bytes, find, hex4, utf8};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, SyncSender};
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub exit: u8,
    pub label: String,
    pub capacity: Option<(u64, u64)>,
}
impl From<&'static str> for Error {
    fn from(label: &'static str) -> Self {
        let exit = match label {
            "Cancelled@source.send"
            | "Cancelled@transform.check"
            | "Cancelled@transform.receive" => 4,
            _ => 2,
        };
        Self {
            exit,
            label: String::literal(label),
            capacity: None,
        }
    }
}
impl From<String> for Error {
    fn from(label: String) -> Self {
        Self {
            exit: 2,
            label,
            capacity: None,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn read_bytes(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut n = 0;
    loop {
        if n == bytes.capacity() {
            let more = if bytes.capacity() == 0 {
                4096
            } else {
                bytes.capacity()
            };
            bytes.reserve(more);
            bytes.resize(bytes.capacity(), 0)
        }
        let cap = bytes.capacity();
        let got = file.read(&mut bytes[n..cap])?;
        if got == 0 {
            break;
        }
        n += got;
    }
    drop(file);
    bytes.truncate(n);
    Ok(bytes)
}
pub fn input() -> Result<(String, std::string::String)> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("Parse@arguments")?;
    let bytes = read_bytes(path).map_err(|_| "Io@input")?;
    if !utf8(&bytes) {
        return Err("Io@input".into());
    }
    Ok((
        String::from_bytes(bytes),
        args.next().unwrap_or_else(|| "normal".into()),
    ))
}
pub fn line<'a>(s: impl Into<View<'a>>, at: &mut usize) -> View<'a> {
    let s = s.into();
    let lo = *at;
    let hi = find(s.as_bytes(), lo, b"\n").unwrap_or(s.len());
    *at = if hi < s.len() { hi + 1 } else { hi };
    let end = if hi > lo && s[hi - 1] == 13 {
        hi - 1
    } else {
        hi
    };
    s.slice(lo, end)
}
pub fn number(s: impl AsRef<[u8]>) -> Result<u64> {
    let s = s.as_ref();
    if s.is_empty() {
        return Err("Parse@number".into());
    }
    let mut value = 0u64;
    let mut big = false;
    for &b in s {
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
pub fn num(out: &mut String, n: impl TryInto<u64>) {
    matched_storage::decimal(out, n.try_into().ok().unwrap())
}
pub fn name(a: impl AsRef<[u8]>, n: impl TryInto<u64>, b: impl AsRef<[u8]>) -> String {
    let mut out = String::copy(a);
    num(&mut out, n);
    out.push_str(b);
    out
}
pub fn admission(variant: impl AsRef<[u8]>, site: impl AsRef<[u8]>, requested: u64) -> Result<()> {
    if View(variant.as_ref())
        .strip_prefix("capacity:")
        .is_some_and(|s| s.as_bytes() == site.as_ref())
    {
        let mut label = String::copy("Capacity@");
        label.push_str(site);
        let requested = if requested > 18446744073709551608 {
            u64::MAX
        } else {
            ((requested + 7) / 8) * 8
        };
        Err(Error {
            exit: 3,
            label,
            capacity: Some((requested, 0)),
        })
    } else {
        Ok(())
    }
}
pub fn put(s: impl AsRef<[u8]>) -> Result<()> {
    std::io::stdout()
        .write_all(s.as_ref())
        .map_err(|_| "Io@stdout".into())
}
pub fn print_error(e: &Error) -> Result<()> {
    put("failure=")?;
    put(&e.label)?;
    put("\n")
}
pub fn exit_status(result: Result<()>) {
    if let Err(e) = result {
        std::process::exit(i32::from(e.exit))
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
    pub fn marker(&self, label: impl AsRef<[u8]>) -> Marker {
        let label = label.as_ref();
        let mut acquire = String::copy("acquire=");
        acquire.push_str(label);
        let mut release = String::copy("release=");
        release.push_str(label);
        let owner = Marker {
            release,
            tx: self.tx.clone(),
        };
        self.tx.try_send(acquire).unwrap();
        owner
    }
    pub fn finish(self, result: Result<()>) {
        let outcome = (|| {
            drop(self.tx);
            for e in self.rx {
                put(&e)?;
                put("\n")?;
            }
            match result {
                Ok(()) => Ok(()),
                Err(e) => {
                    print_error(&e)?;
                    Err(e)
                }
            }
        })();
        exit_status(outcome)
    }
}
impl Drop for Marker {
    fn drop(&mut self) {
        let _ = self.tx.try_send(self.release.clone());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_digits_and_static_errors() {
        for n in [
            0,
            1,
            9,
            10,
            99,
            100,
            999,
            1000,
            9999,
            10000,
            99999999,
            100000000,
            u64::MAX,
        ] {
            let mut s = String::new();
            num(&mut s, n);
            assert_eq!(s.as_bytes(), n.to_string().as_bytes())
        }
        let mut n = 1u64;
        for _ in 1..20 {
            n *= 10;
            for x in [n - 1, n, n + 1] {
                let mut s = String::copy("x=");
                num(&mut s, x);
                assert_eq!(s.as_bytes(), format!("x={x}").as_bytes())
            }
        }
        for prefix in 0..34 {
            let s = format!("{}xabcxxabcy", "_".repeat(prefix));
            for pat in ["x", "abc", "abcy", "z", ""] {
                assert_eq!(find(s.as_bytes(), 0, pat.as_bytes()), s.find(pat))
            }
        }
        let s = String::literal("static");
        assert_eq!(s.capacity(), 0);
        assert_eq!(s.clone().capacity(), 0);
        let e = Error::from("Parse@number");
        assert_eq!(e.label.capacity(), 0);
        assert_eq!(e.exit, 2);
        let mut xs = Vec::new();
        for i in 0..9 {
            xs.push(i)
        }
        assert_eq!(xs.capacity(), 16);
        assert_eq!(&*xs, &[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(utf8("ab𝄞z".as_bytes()));
        assert!(!utf8(&[0xf4, 0x90, 0x80, 0x80]));
        assert!(!utf8(&[0xed, 0xa0, 0x80]));
    }
}
