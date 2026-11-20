//! Part 3 open-loop load generator. Arrivals are Poisson at `rate` turns/s,
//! with one burst (rate * burst_x for burst_len s starting at burst_at s).
//! Each arrival goes to a session (uniform, or skew: half to session 0);
//! a session runs its turns one at a time, and latency is measured from the
//! intended arrival, so queueing behind a slow server counts.
//! Server-added latency = (completion - intended arrival) - tool held ns.
//! Every reply's checksum is checked against the digest table.
//! Usage: loadgen key=value... ; prints one JSON line.
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use units_bench::*;

struct Rec { intended: u64, done: u64, held: u64, ok: bool }

fn main() {
    let kv: HashMap<String, String> = std::env::args().skip(1)
        .map(|a| { let (k, v) = a.split_once('=').unwrap(); (k.to_string(), v.to_string()) }).collect();
    let get = |k: &str, d: &str| kv.get(k).cloned().unwrap_or(d.to_string());
    let port: u16 = get("port", "7700").parse().unwrap();
    let nports: u64 = get("nports", "1").parse().unwrap();
    let sessions: u64 = get("sessions", "256").parse().unwrap();
    let rate: f64 = get("rate", "1000").parse().unwrap();
    let dur: f64 = get("dur", "10").parse().unwrap();
    let warm: f64 = get("warm", "1").parse().unwrap();
    let burst_at: f64 = get("burst_at", "5").parse().unwrap();
    let burst_len: f64 = get("burst_len", "1").parse().unwrap();
    let burst_x: f64 = get("burst_x", "1.5").parse().unwrap();
    let f: u64 = get("f", "0").parse().unwrap();
    let par = get("par", "0") == "1";
    let skew = get("skew", "0") == "1";
    let run: u64 = get("seed", "1").parse().unwrap();
    let threads: usize = get("threads", "4").parse().unwrap();
    let grace: u64 = get("grace", "10").parse::<u64>().unwrap() * 1_000_000_000;
    let table = Arc::new(digest_table(&tool_data()));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    // Connect every session first (the server accepts all before serving).
    let streams: Vec<tokio::net::TcpStream> = rt.block_on(async {
        let mut v = Vec::new();
        for s in 0..sessions {
            let p = port + (s % nports) as u16;
            let t0 = now_ns();
            let mut st = loop {
                match tokio::net::TcpStream::connect(("127.0.0.1", p)).await {
                    Ok(st) => break st,
                    Err(e) => {
                        if now_ns() - t0 > 30_000_000_000 { panic!("connect {p}: {e}"); }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }
            };
            st.set_nodelay(true).unwrap();
            st.write_all(&s.to_le_bytes()).await.unwrap();
            v.push(st);
        }
        v
    });
    let start = now_ns() + 200_000_000;
    let end = start + (dur * 1e9) as u64;
    let deadline = end + grace;
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    for mut st in streams {
        let (tx, mut rx) = mpsc::unbounded_channel::<(Turn, u64)>();
        senders.push(tx);
        let table = table.clone();
        handles.push(rt.spawn(async move {
            let mut recs: Vec<Rec> = Vec::new();
            let mut timeouts = 0u64;
            let mut reply = [0u8; REPLY];
            let mut dead = false;
            while let Some((turn, intended)) = rx.recv().await {
                if dead { timeouts += 1; continue; }
                let left = deadline.saturating_sub(now_ns());
                let io = async {
                    st.write_all(&turn.encode()).await?;
                    st.read_exact(&mut reply).await?;
                    Ok::<(), std::io::Error>(())
                };
                match tokio::time::timeout(std::time::Duration::from_nanos(left), io).await {
                    Ok(Ok(())) => {
                        let done = now_ns();
                        let id = u64::from_le_bytes(reply[0..8].try_into().unwrap());
                        let acc = u64::from_le_bytes(reply[8..16].try_into().unwrap());
                        let held = u64::from_le_bytes(reply[16..24].try_into().unwrap());
                        let ok = id == turn.id && acc == turn.expected(&table);
                        recs.push(Rec { intended, done, held, ok });
                    }
                    _ => { timeouts += 1; dead = true; }
                }
            }
            drop(st);
            (recs, timeouts)
        }));
    }
    // Dispatcher: a plain thread with fine sleeps, so timer granularity does
    // not add to the measured latency.
    let disp = std::thread::spawn(move || {
        let mut x = splitmix(run ^ 0x5EED);
        let mut t = start as f64;
        let mut id = 0u64;
        let mut issued = 0u64;
        loop {
            x = splitmix(x);
            let u = ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
            let rel = (t - start as f64) / 1e9;
            let r = if rel >= burst_at && rel < burst_at + burst_len { rate * burst_x } else { rate };
            t += -u.ln() / r * 1e9;
            if t as u64 >= end { break; }
            x = splitmix(x);
            let s = if skew {
                if x & 1 == 0 { 0 } else { 1 + (x >> 1) % (sessions - 1) }
            } else { x % sessions };
            let now = now_ns();
            if (t as u64) > now + 50_000 {
                std::thread::sleep(std::time::Duration::from_nanos(t as u64 - now - 50_000));
            }
            while now_ns() < t as u64 { std::hint::spin_loop(); }
            let turn = Turn::make(run, id, f, par);
            id += 1;
            issued += 1;
            let _ = senders[s as usize].send((turn, t as u64));
        }
        drop(senders);
        issued
    });
    let issued = disp.join().unwrap();
    let results: Vec<(Vec<Rec>, u64)> = rt.block_on(async {
        let mut v = Vec::new();
        for h in handles { v.push(h.await.unwrap()); }
        v
    });
    let warm_ns = start + (warm * 1e9) as u64;
    let mut lat: Vec<u64> = Vec::new();
    let (mut completed, mut failures, mut timeouts, mut late) = (0u64, 0u64, 0u64, 0u64);
    for (recs, t) in &results {
        timeouts += t;
        for r in recs {
            completed += 1;
            if !r.ok { failures += 1; }
            if r.done > end + 1_000_000_000 { late += 1; }
            if r.intended >= warm_ns {
                lat.push((r.done - r.intended).saturating_sub(r.held));
            }
        }
    }
    lat.sort_unstable();
    let q = |p: f64| if lat.is_empty() { 0 } else { lat[((lat.len() as f64 * p) as usize).min(lat.len() - 1)] };
    println!("{{\"issued\":{issued},\"completed\":{completed},\"failures\":{failures},\"timeouts\":{timeouts},\"late\":{late},\"rate\":{rate},\"measured\":{},\"p50_ns\":{},\"p99_ns\":{},\"p999_ns\":{},\"max_ns\":{}}}",
        lat.len(), q(0.5), q(0.99), q(0.999), lat.last().copied().unwrap_or(0));
}
