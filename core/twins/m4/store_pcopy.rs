//! The memory store's target twin (STORE-SPEC v2.13 §6, Q3): the work the store must do per commit,
//! over the persistent path-copying B+ tree with the spec's fan-out in `store_tree.rs` (which states
//! the layout, what is shared, and the work per commit, put and fetch).
//!
//! `m4-store-pcopy R1 <gate>`: R1. 100,000 commits, each putting Account k = i and fetching it back.
//! Prints `DONE 5000750000 <ns>`. After the gate: version 100,000, 100,000 rows, and the account at
//! 110 B per row (§1.11: 62 + key 12 → 16 + value 30 → 32).
//!
//! `m4-store-pcopy 0|1|3 <gate>`: the indexed overwrite (store-build-1010's `index_bench.hirsel`,
//! ticket 5052). Table "accounts" with 0, 1 or 3 indexes on `id % 4`; 100,000 Accounts populated
//! (k, balance k) before READY; then 10,000 commits each putting Account k = 0..9999 with balance
//! k + 100,000 and returning the balance, no fetch. Each overwrite decodes the replaced row and
//! compares each index's old and new key (equal here, so no index entry is written, J1). Prints
//! `DONE 1049995000 <ns>`. After the gate: every overwritten row read back, 100,000 × (1 + indexes)
//! tree rows, version 110,000, and the account at 110 + 86 × indexes B per row.
//!
//! Account is id u64, balance i64, owner length u32 and "owner000": 28 bytes (main's codec since 07c3aa57: String counts are u32). Values carry schema
//! number 0 (u16 LE), keys table id 1 (u32 BE) then u64 BE, as store_mem.rs.
#[path = "store_tree.rs"]
#[allow(
    dead_code,
    reason = "shared with store_journal.rs; each binary uses its own subset"
)]
mod tree;
use std::io::{Read, Write};
use std::time::Instant;
use tree::{Account, Db, Memory, Table, fetch, put, run};

fn owner() -> String {
    String::from("owner000")
}
fn insert_row(db: &mut Db, t: &Table, i: u64) -> u64 {
    let done = run(db, &mut Memory, |tx| {
        let a = Account {
            id: i,
            owner: owner(),
            balance: i64::try_from(i).unwrap(),
        };
        put(tx, t, i, &a)?;
        let back = fetch(tx, t, i)?.unwrap();
        if back.id != i || back.balance != i64::try_from(i).unwrap() || back.owner != "owner000" {
            panic!("store R1 wrong row");
        }
        Ok(back.id + u64::try_from(back.owner.len()).unwrap())
    });
    match done {
        Ok(d) => {
            assert!(d.version == i + 1 && d.reads.len() == 1 && d.at > 0);
            d.value
        }
        Err(e) => panic!("store R1 refused: {e:?}"),
    }
}
fn save(db: &mut Db, t: &Table, k: u64, balance: u64) -> u64 {
    let done = run(db, &mut Memory, |tx| {
        let a = Account {
            id: k,
            owner: owner(),
            balance: i64::try_from(balance).unwrap(),
        };
        put(tx, t, k, &a)?;
        Ok(balance)
    });
    match done {
        Ok(d) => d.value,
        Err(e) => panic!("indexed put refused: {e:?}"),
    }
}
fn bucket(a: &Account) -> u64 {
    a.id % 4
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let mut gate = std::fs::File::open(args.next().unwrap()).unwrap();
    let indexes: &[fn(&Account) -> u64] = match mode.as_str() {
        "R1" | "0" => &[],
        "1" => &[bucket],
        "3" => &[bucket, bucket, bucket],
        _ => panic!("modes: R1, 0, 1, 3"),
    };
    let (mut db, t) = tree::open(indexes);
    let catalog = db.resident;
    if mode != "R1" {
        for k in 0..100000 {
            save(&mut db, &t, k, k);
        }
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let start = Instant::now();
    let sum: u64 = if mode == "R1" {
        (0..100000).map(|i| insert_row(&mut db, &t, i)).sum()
    } else {
        (0..10000).map(|k| save(&mut db, &t, k, k + 100000)).sum()
    };
    let elapsed = start.elapsed().as_nanos();
    println!("DONE {sum} {elapsed}");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let n = indexes.len() as u64;
    let rows = tree::rows(&db);
    assert_eq!(rows.len() as u64, 100000 * (1 + n));
    assert_eq!(db.resident, catalog + 100000 * (110 + 86 * n));
    if mode == "R1" {
        assert_eq!(db.version, 100000);
    } else {
        assert_eq!(db.version, 110000);
        for (i, r) in rows.iter().take(100000).enumerate() {
            let k = i as u64;
            let a = tree::decode(&r.value);
            let balance = if k < 10000 { k + 100000 } else { k };
            assert!(
                a.id == k && a.balance == i64::try_from(balance).unwrap() && a.owner == "owner000"
            );
        }
    }
}
