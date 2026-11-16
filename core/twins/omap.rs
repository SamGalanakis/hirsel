//! Phase-separated twin; population and final drops lie outside READY/DONE.
use std::{collections::BTreeMap, hint::black_box, io::{Read, Write}, sync::Arc, time::Instant};

fn phase(mut operation: impl FnMut() -> u64, gate: &mut std::fs::File) {
    println!("READY");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let start = Instant::now();
    let answer = operation();
    let nanos = start.elapsed().as_nanos();
    println!("DONE {answer} {nanos}");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
}

fn btree(mode: &str, gate: &mut std::fs::File) {
    let mut m = BTreeMap::new();
    let mut held = Vec::with_capacity(1_000);
    if mode != "insert" && mode != "unique" && mode != "bulk" { for i in 0..100_000u64 { m.insert(i, i + 1); } }
    let input: Vec<_> = if mode == "bulk" { (0..100_000u64).map(|i| (i, i + 1)).collect() } else { Vec::new() };
    phase(|| match mode {
        "bulk" => { m = input.iter().copied().collect(); m.len() as u64 },
        "insert" | "unique" => { for i in 0..100_000u64 { m.insert(i, i + 1); } m.len() as u64 },
        "lookup" => (0..100_000).map(|i| black_box(&m)[&((i * 65537) % 100_000)]).sum(),
        "range" => m.range(25_000..75_000).map(|(k, v)| k + v).sum(),
        "snapshots" => {
            for i in 0..1_000 { held.push(m.clone()); m.insert(i, 100_000 + i); }
            (0..1_000).map(|i| held[i as usize][&i]).sum()
        },
        _ => panic!("unknown workload"),
    }, gate);
    black_box((&m, &held, &input));
}

fn persistent(mode: &str, unique: bool, gate: &mut std::fs::File) {
    let mut m = im::OrdMap::new();
    let mut held = Vec::with_capacity(1_000);
    if mode != "insert" && mode != "unique" && mode != "bulk" { for i in 0..100_000u64 { m.insert(i, i + 1); } }
    let input: Vec<_> = if mode == "bulk" { (0..100_000u64).map(|i| (i, i + 1)).collect() } else { Vec::new() };
    phase(|| match mode {
        "bulk" => { m = input.iter().copied().collect(); m.len() as u64 },
        "insert" | "unique" => {
            for i in 0..100_000u64 {
                if unique || mode == "unique" { m.insert(i, i + 1); }
                else { let pinned = m.clone(); m.insert(i, i + 1); black_box(&pinned); }
            }
            m.len() as u64
        },
        "lookup" => (0..100_000).map(|i| black_box(&m)[&((i * 65537) % 100_000)]).sum(),
        "range" => m.range(25_000..75_000).map(|(k, v)| k + v).sum(),
        "snapshots" => {
            for i in 0..1_000 { held.push(m.clone()); m.insert(i, 100_000 + i); }
            (0..1_000).map(|i| held[i as usize][&i]).sum()
        },
        _ => panic!("unknown workload"),
    }, gate);
    black_box((&m, &held, &input));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let implementation = args.next().unwrap();
    let mode = args.next().unwrap();
    let mut gate = std::fs::File::open(args.next().unwrap()).unwrap();
    if mode == "unwrap" {
        let mut holder = Some(Arc::new((0u64, (0..32u64).map(Arc::new).collect::<Vec<_>>())));
        phase(|| {
            for _ in 0..100_000 {
                let mut h = holder.take().unwrap();
                if implementation == "arc-rewrap" {
                    let mut node = Arc::try_unwrap(h).unwrap();
                    node.0 += 1;
                    h = Arc::new(node);
                } else { Arc::get_mut(&mut h).unwrap().0 += 1; }
                black_box(&h);
                holder = Some(h);
            }
            holder.as_ref().unwrap().0
        }, &mut gate);
        black_box(&holder);
        return;
    }
    match implementation.as_str() {
        "btree" => btree(&mode, &mut gate),
        "im" => persistent(&mode, false, &mut gate),
        "im-unique" => persistent(&mode, true, &mut gate),
        _ => panic!("unknown implementation"),
    }
}
