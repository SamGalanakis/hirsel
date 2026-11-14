//! Idiomatic twin of core/programs/m2b/tx `bench(100000)`: n transactions over a
//! HashMap<u64, Account> of 1000 accounts. Each body runs under `catch_unwind`, reads
//! through a transaction that records keys in a `Vec` read set, and writes into its own
//! HashMap; a body that returns Ok is committed by reserving for its new keys and then
//! inserting. Every tenth body overflows (a checked add that panics). Prints the same
//! checksum: payers' new balances, a million per failure, and every recorded read.
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};

#[derive(Clone)]
struct Account {
    id: u64,
    owner: String,
    balance: i64,
}

enum Abort {
    Insufficient,
    NoRow,
}

struct Tx<'a> {
    base: &'a HashMap<u64, Account>,
    writes: HashMap<u64, Account>,
    reads: &'a mut Vec<u64>,
}

impl Tx<'_> {
    fn read(&mut self, k: u64) -> Option<&Account> {
        self.reads.push(k);
        self.writes.get(&k).or_else(|| self.base.get(&k))
    }

    fn write(&mut self, k: u64, v: Account) {
        self.writes.insert(k, v);
    }
}

enum Outcome<R> {
    Committed(R),
    Aborted,
    Failed,
}

fn run<R>(
    db: &mut HashMap<u64, Account>,
    reads: &mut Vec<u64>,
    body: impl FnOnce(&mut Tx) -> Result<R, Abort>,
) -> Outcome<R> {
    let caught = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut tx = Tx { base: db, writes: HashMap::new(), reads };
        body(&mut tx).map(|r| (r, tx.writes))
    }));
    match caught {
        Err(_) => Outcome::Failed,
        Ok(Err(_)) => Outcome::Aborted,
        Ok(Ok((r, writes))) => {
            let fresh = writes.keys().filter(|k| !db.contains_key(k)).count();
            db.reserve(fresh);
            db.extend(writes);
            Outcome::Committed(r)
        }
    }
}

fn seen(tx: &mut Tx, k: u64) -> Result<i64, Abort> {
    tx.read(k).map(|a| a.balance).ok_or(Abort::NoRow)
}

fn row(id: u64, balance: i64) -> Account {
    Account { id, owner: String::new(), balance }
}

fn transfer(tx: &mut Tx, (from, to, amount): (u64, u64, i64)) -> Result<u64, Abort> {
    let paid = seen(tx, from)?;
    let got = seen(tx, to)?;
    if paid < amount {
        return Err(Abort::Insufficient);
    }
    tx.write(from, row(from, paid - amount));
    tx.write(to, row(to, got + amount));
    Ok((paid - amount) as u64)
}

fn poison(tx: &mut Tx, (from, to, amount): (u64, u64, i64)) -> Result<u64, Abort> {
    let paid = seen(tx, from)?;
    let got = seen(tx, to)?;
    let total = (paid + got + amount).checked_add(i64::MAX).expect("overflow");
    Ok(total as u64)
}

fn bench(n: u64) -> u64 {
    let mut db: HashMap<u64, Account> = (1..=1000).map(|id| (id, row(id, 1000))).collect();
    let mut sum = 0u64;
    for i in 0..n {
        let arg = (i % 1000 + 1, (i * 7 + 3) % 1000 + 1, 1i64);
        let mut reads = Vec::new();
        let o = if i % 10 == 9 {
            run(&mut db, &mut reads, |tx| poison(tx, arg))
        } else {
            run(&mut db, &mut reads, |tx| transfer(tx, arg))
        };
        match o {
            Outcome::Committed(r) => sum += r,
            Outcome::Failed => sum += 1_000_000,
            Outcome::Aborted => return 0,
        }
        sum += reads.len() as u64;
    }
    assert!(db.values().all(|a| a.id > 0 && a.owner.is_empty()));
    sum
}

fn main() {
    panic::set_hook(Box::new(|_| {}));
    println!("{}", bench(100_000));
}
