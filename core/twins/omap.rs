//! Ordered-map twin: std BTreeMap with clone-per-snapshot, and im 15.1 OrdMap.
//! Each invocation includes the same ascending 100k-row setup as Hirsel.
use std::{collections::BTreeMap, hint::black_box};

fn btree(mode: &str) -> u64 {
    let mut m = BTreeMap::new();
    for i in 0..100_000u64 {
        m.insert(i, i + 1);
    }
    let answer = match mode {
        "insert" => m.len() as u64,
        "lookup" => (0..100_000).map(|i| m[&((i * 65537) % 100_000)]).sum(),
        "range" => m.range(25_000..75_000).map(|(k, v)| k + v).sum(),
        "snapshots" => {
            let mut held = Vec::with_capacity(1_000);
            for i in 0..1_000 {
                held.push(m.clone());
                m.insert(i, 100_000 + i);
            }
            let sum = (0..1_000).map(|i| held[i as usize][&i]).sum();
            black_box(&held);
            sum
        }
        _ => panic!("unknown workload"),
    };
    black_box(&m);
    answer
}

fn persistent(mode: &str, unique: bool) -> u64 {
    let mut m = im::OrdMap::new();
    for i in 0..100_000u64 {
        if unique {
            m.insert(i, i + 1);
        } else {
            let pinned = m.clone();
            m.insert(i, i + 1);
            black_box(&pinned);
        }
    }
    let answer = match mode {
        "insert" => m.len() as u64,
        "lookup" => (0..100_000).map(|i| m[&((i * 65537) % 100_000)]).sum(),
        "range" => m.range(25_000..75_000).map(|(k, v)| k + v).sum(),
        "snapshots" => {
            let mut held = Vec::with_capacity(1_000);
            for i in 0..1_000 {
                held.push(m.clone());
                m.insert(i, 100_000 + i);
            }
            let sum = (0..1_000).map(|i| held[i as usize][&i]).sum();
            black_box(&held);
            sum
        }
        _ => panic!("unknown workload"),
    };
    black_box(&m);
    answer
}

fn main() {
    let mut args = std::env::args().skip(1);
    let implementation = args.next().expect("btree | im | im-unique");
    let mode = args.next().expect("insert | lookup | range | snapshots");
    let answer = match implementation.as_str() {
        "btree" => btree(&mode),
        "im" => persistent(&mode, false),
        "im-unique" => persistent(&mode, true),
        _ => panic!("unknown implementation"),
    };
    let expected = match mode.as_str() {
        "insert" => 100_000,
        "lookup" => 5_000_050_000,
        "range" => 5_000_000_000,
        "snapshots" => 500_500,
        _ => unreachable!(),
    };
    assert_eq!(answer, expected);
    println!("{answer}");
}
