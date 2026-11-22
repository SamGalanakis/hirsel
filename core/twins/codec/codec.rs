//! Idiomatic twin of core/twins/codec/codec.hirsel: the std.codec format by hand.
//! `codec WORK MODE N R`: WORK 0 is N rows {id: u64, owner: String, balance: i64}, 1 is
//! N f64s, 2 is N strings, each one List; MODE 0 builds and encodes once, 1 also
//! re-encodes R times, 2 also decodes R times with std.codec's checks (bounds,
//! UTF-8, canonical NaN). Prints the same checksum as the Hirsel program; the check
//! reads only the count and the last value, so a decode pass costs the decode alone.
use std::env;

struct Account {
    id: u64,
    owner: String,
    balance: i64,
}

struct Truncated;

trait Codec: Sized {
    const LEAST: usize;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated>;
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated>;
}

fn word(b: &[u8], at: &mut usize) -> Result<u64, Truncated> {
    let bytes: [u8; 8] = b
        .get(*at..*at + 8)
        .ok_or(Truncated)?
        .try_into()
        .map_err(|_| Truncated)?;
    *at += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn count(b: &[u8], at: &mut usize) -> Result<u32, Truncated> {
    let bytes: [u8; 4] = b
        .get(*at..*at + 4)
        .ok_or(Truncated)?
        .try_into()
        .map_err(|_| Truncated)?;
    *at += 4;
    Ok(u32::from_le_bytes(bytes))
}

impl Codec for u64 {
    const LEAST: usize = 8;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        out.extend_from_slice(&self.to_le_bytes());
        Ok(())
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        word(b, at)
    }
}

impl Codec for i64 {
    const LEAST: usize = 8;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        out.extend_from_slice(&self.to_le_bytes());
        Ok(())
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        Ok(word(b, at)? as i64)
    }
}

impl Codec for f64 {
    const LEAST: usize = 8;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        out.extend_from_slice(&self.to_bits().to_le_bytes());
        Ok(())
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        let x = word(b, at)?;
        if x & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
            && x & 0xf_ffff_ffff_ffff != 0
            && x != 0x7ff8_0000_0000_0000
        {
            return Err(Truncated);
        }
        Ok(f64::from_bits(x))
    }
}

impl Codec for String {
    const LEAST: usize = 4;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        let n = u32::try_from(self.len()).map_err(|_| Truncated)?;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(self.as_bytes());
        Ok(())
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        let n = usize::try_from(count(b, at)?).map_err(|_| Truncated)?;
        let bytes = b.get(*at..).ok_or(Truncated)?.get(..n).ok_or(Truncated)?;
        let s = String::from_utf8(bytes.to_vec()).map_err(|_| Truncated)?;
        *at += n;
        Ok(s)
    }
}

impl<T: Codec> Codec for Vec<T> {
    const LEAST: usize = 4;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        let n = u32::try_from(self.len()).map_err(|_| Truncated)?;
        out.extend_from_slice(&n.to_le_bytes());
        for x in self {
            x.encode(out)?;
        }
        Ok(())
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        let n = usize::try_from(count(b, at)?).map_err(|_| Truncated)?;
        if n > (b.len() - *at) / T::LEAST {
            return Err(Truncated);
        }
        let mut xs = Vec::with_capacity(n);
        for _ in 0..n {
            xs.push(T::decode(b, at)?);
        }
        Ok(xs)
    }
}

impl Codec for Account {
    const LEAST: usize = 20;
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Truncated> {
        self.id.encode(out)?;
        self.owner.encode(out)?;
        self.balance.encode(out)
    }
    fn decode(b: &[u8], at: &mut usize) -> Result<Self, Truncated> {
        let id = u64::decode(b, at)?;
        let owner = String::decode(b, at)?;
        let balance = i64::decode(b, at)?;
        Ok(Account { id, owner, balance })
    }
}

fn run<T: Codec>(xs: &T, mode: u64, r: u64, check: fn(&T) -> u64) -> u64 {
    let mut out = Vec::new();
    if xs.encode(&mut out).is_err() {
        return 0;
    }
    let mut sum = out.len() as u64;
    for _ in 0..r {
        if mode == 1 {
            out.clear();
            if xs.encode(&mut out).is_err() {
                return 0;
            }
            sum += out.len() as u64;
        } else if mode == 2 {
            let mut at = 0;
            match T::decode(&out, &mut at) {
                Ok(back) if at == out.len() => sum += check(&back),
                _ => return 0,
            }
        }
    }
    sum
}

fn owner(k: u64) -> String {
    format!("owner-{k}")
}

fn main() {
    let a: Vec<u64> = env::args()
        .skip(1)
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    let (work, mode, n, r) = (a[0], a[1], a[2], a[3]);
    let v = match work {
        // 3 is Hirsel's other route to the same bytes.
        0 | 3 => {
            let xs: Vec<Account> = (0..n)
                .map(|k| Account {
                    id: k,
                    owner: owner(k),
                    balance: k as i64 - 50000,
                })
                .collect();
            run(&xs, mode, r, |xs| {
                let a = &xs[xs.len() - 1];
                xs.len() as u64 + a.id + a.owner.len() as u64
            })
        }
        1 => {
            let xs: Vec<f64> = (0..n).map(|k| k as f64 * 0.37 + 1.5).collect();
            run(&xs, mode, r, |xs| {
                xs.len() as u64 + (xs[xs.len() - 1].to_bits() & 0xff)
            })
        }
        _ => {
            let xs: Vec<String> = (0..n).map(owner).collect();
            run(&xs, mode, r, |xs| {
                xs.len() as u64 + xs[xs.len() - 1].len() as u64
            })
        }
    };
    println!("{v}");
}
