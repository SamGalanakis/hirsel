//! Idiomatic twin of core/programs/m2b/table `bench(100000)`: n rows put into a
//! HashMap<u64, Account>, each encoded in key order into one buffer with the same
//! hand codec (id, balance, owner length as little-endian u64, then owner bytes),
//! then all decoded back. Prints the same checksum: the sum of id + owner length.
use std::collections::HashMap;

struct Account {
    id: u64,
    owner: String,
    balance: i64,
}

fn put_u64(x: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn encode(a: &Account, out: &mut Vec<u8>) {
    put_u64(a.id, out);
    put_u64(a.balance as u64, out);
    put_u64(a.owner.len() as u64, out);
    out.extend_from_slice(a.owner.as_bytes());
}

fn get_u64(b: &[u8], at: &mut usize) -> Option<u64> {
    let bytes: [u8; 8] = b.get(*at..)?.get(..8)?.try_into().ok()?;
    *at += 8;
    Some(u64::from_le_bytes(bytes))
}

fn decode(b: &[u8], at: &mut usize) -> Option<Account> {
    let mut p = *at;
    let id = get_u64(b, &mut p)?;
    let balance = get_u64(b, &mut p)? as i64;
    let n = usize::try_from(get_u64(b, &mut p)?).ok()?;
    if n > b.len() - p {
        return None;
    }
    let owner = String::from_utf8(b[p..p + n].to_vec()).ok()?;
    *at = p + n;
    Some(Account { id, owner, balance })
}

fn bench(n: u64) -> Option<u64> {
    let mut t: HashMap<u64, Account> = HashMap::new();
    for k in 0..n {
        t.insert(k, Account { id: k, owner: String::from("owner"), balance: i64::try_from(k).ok()? });
    }
    let mut out = Vec::new();
    for k in 0..n {
        encode(t.get(&k)?, &mut out);
    }
    let mut at = 0;
    let mut sum: u64 = 0;
    while at < out.len() {
        let a = decode(&out, &mut at)?;
        sum += a.id + a.owner.len() as u64;
        let _ = a.balance;
    }
    Some(sum)
}

fn main() {
    println!("{}", bench(100_000).unwrap_or(0));
}
