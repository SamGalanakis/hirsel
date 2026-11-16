//! store-r1-cost-1030 local fairness twin: store_mem.rs with im::OrdMap (persistent, path-copying).
//! R1 keeps the base version alive across each put, as a transaction's rollback point does;
//! R1U inserts with no other holder (in place).
//! Account is id u64, balance i64, owner length u64 and "owner000": 32 bytes.
//! Values carry schema number 0 (u16 LE), keys table id 1 (u32 BE) then u64 BE.
use std::{
    io::{Read, Write},
    sync::Arc,
    time::Instant,
};

struct Account {
    id: u64,
    owner: String,
    balance: i64,
}
fn key(i: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&1u32.to_be_bytes());
    b.extend_from_slice(&i.to_be_bytes());
    b
}
fn encode(a: &Account) -> Vec<u8> {
    let mut b = Vec::with_capacity(34);
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&a.id.to_le_bytes());
    b.extend_from_slice(&a.balance.to_le_bytes());
    b.extend_from_slice(&u64::try_from(a.owner.len()).unwrap().to_le_bytes());
    b.extend_from_slice(a.owner.as_bytes());
    b
}
fn decode(b: &[u8]) -> Account {
    assert_eq!(u16::from_le_bytes(b[0..2].try_into().unwrap()), 0);
    let id = u64::from_le_bytes(b[2..10].try_into().unwrap());
    let balance = i64::from_le_bytes(b[10..18].try_into().unwrap());
    let n = usize::try_from(u64::from_le_bytes(b[18..26].try_into().unwrap())).unwrap();
    let owner = String::from_utf8(b[26..26 + n].to_vec()).unwrap();
    assert_eq!(26 + n, b.len());
    Account { id, owner, balance }
}
fn observed(a: Account, k: u64) -> u64 {
    assert_eq!(a.id, k);
    assert_eq!(a.balance, i64::try_from(k).unwrap());
    assert_eq!(a.owner, "owner000");
    a.id + u64::try_from(a.owner.len()).unwrap()
}
fn put(m: &mut im::OrdMap<Vec<u8>, Vec<u8>>, i: u64) -> u64 {
    let a = Account {
        id: i,
        owner: String::from("owner000"),
        balance: i64::try_from(i).unwrap(),
    };
    m.insert(key(i), encode(&a));
    observed(decode(&m[&key(i)]), i)
}
fn sm(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}
fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let mut gate = std::fs::File::open(args.next().unwrap()).unwrap();
    let mut m = im::OrdMap::new();
    if mode == "R2" || mode == "R3" {
        for i in 0..100000 {
            put(&mut m, i);
        }
    }
    let mut map = Arc::new(m);
    println!("READY");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let start = Instant::now();
    let sum: u64 = match mode.as_str() {
        "R1" => {
            let m = Arc::get_mut(&mut map).unwrap();
            (0..100000)
                .map(|i| {
                    let base = m.clone();
                    let r = put(m, i);
                    drop(base);
                    r
                })
                .sum()
        }
        "R1U" => {
            let m = Arc::get_mut(&mut map).unwrap();
            (0..100000).map(|i| put(m, i)).sum()
        }
        "P1" | "P100" => {
            let m = Arc::get_mut(&mut map).unwrap();
            (0..100000).map(|i| put(m, i)).sum()
        }
        "R0" => 0,
        "R2" => map
            .range::<_, Vec<u8>>(..)
            .map(|(k, v)| observed(decode(v), u64::from_be_bytes(k[4..12].try_into().unwrap())))
            .sum(),
        "R3" => {
            let held = Arc::clone(&map);
            std::thread::spawn(move || {
                let mut state: u64 = 1;
                let mut sum = 0u64;
                for _ in 0..100000 {
                    let k = sm(state) % 100000;
                    state = state.wrapping_add(0x9E3779B97F4A7C15);
                    sum += observed(decode(&held[&key(k)]), k);
                }
                sum
            })
            .join()
            .unwrap()
        }
        _ => panic!("unknown store row"),
    };
    let elapsed = start.elapsed().as_nanos();
    println!("DONE {sum} {elapsed}");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    assert_eq!(map.len(), if mode == "R0" { 0 } else { 100000 });
}
