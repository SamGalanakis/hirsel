//! Part 3 open-loop load generator. Arrivals are Poisson at `rate` turns/s,
//! with one burst (rate * burst_x for burst_len s starting at burst_at s).
//! The dispatcher offers every arrival independently of the replies. In
//! pipeline mode each connection also writes independently of its reader;
//! otherwise the queued turn still carries its original intended time.
//! End-to-end latency is the full interval from intended arrival to reply.
//! Server-added latency excludes per-connection queueing and tool hold time.
//! Every reply's checksum is checked against the digest table.
//! Usage: loadgen key=value... ; prints one JSON line.
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::sync::{Arc, atomic::{AtomicU64, Ordering::Relaxed}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Barrier, mpsc};
use units_bench::*;

// `ready`: when this turn could first be sent, the later of its intended
// arrival and the reply to the session's previous turn.
struct Rec { id: u64, intended: u64, ready: u64, sent: u64, done: u64, held: u64, ok: bool }

enum Work {
    Turn(Turn, u64),
    Replace(u64),
}

struct Churn {
    live: AtomicU64,
    minimum_live: AtomicU64,
    completed: AtomicU64,
    completed_in_window: AtomicU64,
}

async fn connect(port: u16, client: u64) -> tokio::net::TcpStream {
    let started = now_ns();
    let mut stream = loop {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => break stream,
            Err(error) => {
                if now_ns() - started > 30_000_000_000 { panic!("connect {port}: {error}"); }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    };
    stream.set_nodelay(true).unwrap();
    stream.write_all(&client.to_le_bytes()).await.unwrap();
    stream
}

async fn ready(stream: &mut tokio::net::TcpStream, client: u64) {
    let mut ack = [0; 8];
    stream.read_exact(&mut ack).await.unwrap();
    assert_eq!(u64::from_le_bytes(ack), client);
}

fn wait_for(target: u64) {
    let now = now_ns();
    if target > now + 50_000 {
        std::thread::sleep(std::time::Duration::from_nanos(target - now - 50_000));
    }
    while now_ns() < target { std::hint::spin_loop(); }
}

async fn pipeline(
    stream: &mut tokio::net::TcpStream,
    mut work: mpsc::UnboundedReceiver<Work>,
    table: &[[u64; 3]],
    deadline: u64,
) -> (Vec<Rec>, u64) {
    let (mut reader, mut writer) = stream.split();
    let (sent, mut pending) = mpsc::unbounded_channel();
    let write = async move {
        while let Some(Work::Turn(turn, intended)) = work.recv().await {
            let started = now_ns();
            writer.write_all(&turn.encode()).await?;
            if sent.send((turn, intended, started)).is_err() {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    let mut recs = Vec::new();
    let read = async {
        let mut prev_done = 0;
        let mut reply = [0; REPLY];
        while let Some((turn, intended, started)) = pending.recv().await {
            reader.read_exact(&mut reply).await?;
            let done = now_ns();
            let (held, ok) = check_reply(&reply, turn, table);
            recs.push(Rec { id: turn.id, intended, ready: intended.max(prev_done), sent: started, done, held, ok });
            prev_done = done;
        }
        Ok::<(), std::io::Error>(())
    };
    let until = tokio::time::Instant::now() + std::time::Duration::from_nanos(deadline.saturating_sub(now_ns()));
    let result = tokio::time::timeout_at(until, async { tokio::try_join!(write, read) }).await;
    (recs, u64::from(!matches!(result, Ok(Ok(_)))))
}

fn check_reply(reply: &[u8; REPLY], turn: Turn, table: &[[u64; 3]]) -> (u64, bool) {
    let id = u64::from_le_bytes(reply[0..8].try_into().unwrap());
    let acc = u64::from_le_bytes(reply[8..16].try_into().unwrap());
    let held = u64::from_le_bytes(reply[16..24].try_into().unwrap());
    (held, id == turn.id && acc == turn.expected(table))
}

fn main() {
    let kv: HashMap<String, String> = std::env::args().skip(1)
        .map(|a| { let (k, v) = a.split_once('=').unwrap(); (k.to_string(), v.to_string()) }).collect();
    let get = |k: &str, d: &str| kv.get(k).cloned().unwrap_or(d.to_string());
    let port: u16 = get("port", "7700").parse().unwrap();
    let sessions: u64 = get("sessions", "256").parse().unwrap();
    let ready_ack = get("ready_ack", "0") == "1";
    let reserved_hot: u64 = get("reserved_hot", "0").parse().unwrap();
    let hot = get("hot", "0") == "1";
    let close_after_drain = get("close_after_drain", "0") == "1";
    let pipelined = get("pipeline", "0") == "1";
    let detail = kv.contains_key("slow_turns");
    let churn_per_s: u64 = get("churn_per_s", "0").parse().unwrap();
    let rate: f64 = get("rate", "1000").parse().unwrap();
    let dur: f64 = get("dur", "10").parse().unwrap();
    let warm: f64 = get("warm", "1").parse().unwrap();
    let burst_at: f64 = get("burst_at", "5").parse().unwrap();
    let burst_len: f64 = get("burst_len", "1").parse().unwrap();
    let burst_x: f64 = get("burst_x", "1.5").parse().unwrap();
    let f: u64 = get("f", "0").parse().unwrap();
    let par = get("par", "0") == "1";
    let skew_clients: u64 = get("skew_clients", &get("skew", "0")).parse().unwrap();
    let run: u64 = get("seed", "1").parse().unwrap();
    let threads: usize = get("threads", "4").parse().unwrap();
    let grace: u64 = get("grace", "10").parse::<u64>().unwrap() * 1_000_000_000;
    assert!(reserved_hot < sessions && (!hot || (ready_ack && reserved_hot > 0)));
    assert!(skew_clients < sessions && (reserved_hot == 0 || skew_clients == 0));
    assert!(churn_per_s == 0 || (ready_ack && reserved_hot == 0 && !close_after_drain));
    assert!(!pipelined || (!hot && churn_per_s == 0));
    let table = Arc::new(digest_table(&tool_data()));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    // Connect every session first (the server accepts all before serving).
    let streams: Vec<tokio::net::TcpStream> = rt.block_on(async {
        let mut v = Vec::new();
        for s in 0..sessions {
            v.push(connect(port, s).await);
        }
        if ready_ack {
            // The server replies after setting up the session, including its
            // tool connections. Keep all connections open until the run.
            for (sid, st) in v.iter_mut().enumerate() {
                ready(st, sid as u64).await;
            }
        }
        v
    });
    let start = now_ns() + 200_000_000;
    let end = start + (dur * 1e9) as u64;
    let warm_ns = start + (warm * 1e9) as u64;
    let deadline = end + grace;
    let replacements = u64::try_from(u128::from(end - start) * u128::from(churn_per_s) / 1_000_000_000).unwrap();
    let churn = (churn_per_s != 0).then(|| Arc::new(Churn {
        live: AtomicU64::new(sessions), minimum_live: AtomicU64::new(sessions),
        completed: AtomicU64::new(0), completed_in_window: AtomicU64::new(0),
    }));
    if let Some(phase_file) = kv.get("phase_file") {
        let tmp = format!("{phase_file}.tmp");
        std::fs::write(&tmp, start.to_string()).unwrap();
        std::fs::rename(tmp, phase_file).unwrap();
    }
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    let drain = close_after_drain.then(|| Arc::new(Barrier::new(sessions as usize)));
    for (sid, mut st) in streams.into_iter().enumerate() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Work>();
        senders.push(tx);
        let table = table.clone();
        let drain = drain.clone();
        let churn = churn.clone();
        let is_hot = hot && (sid as u64) < reserved_hot;
        handles.push(rt.spawn(async move {
            if pipelined {
                let (recs, timeouts) = pipeline(&mut st, rx, &table, deadline).await;
                if let Some(drain) = drain {
                    drain.wait().await;
                }
                drop(st);
                return (recs, timeouts, 0, if detail { now_ns() } else { 0 });
            }
            let mut recs: Vec<Rec> = Vec::new();
            let mut timeouts = 0u64;
            let mut reply = [0u8; REPLY];
            let mut dead = false;
            let mut prev_done = 0u64;
            let mut hot_issued = 0u64;
            if is_hot {
                let delay = start.saturating_sub(now_ns());
                tokio::time::sleep(std::time::Duration::from_nanos(delay)).await;
            }
            loop {
                let (turn, intended) = if is_hot {
                    let intended = if hot_issued == 0 { start } else { prev_done };
                    if dead || now_ns() >= end { break; }
                    let id = (1 << 63) | ((sid as u64) << 32) | hot_issued;
                    hot_issued += 1;
                    (Turn::make(run, id, f, par), intended)
                } else {
                    match rx.recv().await {
                        Some(Work::Turn(turn, intended)) => (turn, intended),
                        Some(Work::Replace(client)) => {
                            let churn = churn.as_ref().unwrap();
                            let live = churn.live.fetch_sub(1, Relaxed) - 1;
                            churn.minimum_live.fetch_min(live, Relaxed);
                            drop(st);
                            st = connect(port, client).await;
                            ready(&mut st, client).await;
                            churn.live.fetch_add(1, Relaxed);
                            churn.completed.fetch_add(1, Relaxed);
                            if (warm_ns..end).contains(&now_ns()) {
                                churn.completed_in_window.fetch_add(1, Relaxed);
                            }
                            dead = false;
                            continue;
                        }
                        None => break,
                    }
                };
                if dead { timeouts += 1; continue; }
                let sent = now_ns();
                let left = deadline.saturating_sub(sent);
                let io = async {
                    st.write_all(&turn.encode()).await?;
                    st.read_exact(&mut reply).await?;
                    Ok::<(), std::io::Error>(())
                };
                match tokio::time::timeout(std::time::Duration::from_nanos(left), io).await {
                    Ok(Ok(())) => {
                        let done = now_ns();
                        let (held, ok) = check_reply(&reply, turn, &table);
                        recs.push(Rec { id: turn.id, intended, ready: intended.max(prev_done), sent, done, held, ok });
                        prev_done = done;
                    }
                    _ => { timeouts += 1; dead = true; }
                }
            }
            if let Some(drain) = drain {
                drain.wait().await;
            }
            drop(st);
            (recs, timeouts, hot_issued, if detail { now_ns() } else { 0 })
        }));
    }
    // Dispatcher: a plain thread with fine sleeps, so timer granularity does
    // not add to the measured latency.
    let disp = std::thread::spawn(move || {
        let mut x = splitmix(run ^ 0x5EED);
        let mut t = start as f64;
        let mut id = 0u64;
        let mut issued = 0u64;
        let mut replaced = 0u64;
        loop {
            x = splitmix(x);
            let u = ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
            let rel = (t - start as f64) / 1e9;
            let r = if rel >= burst_at && rel < burst_at + burst_len { rate * burst_x } else { rate };
            t += -u.ln() / r * 1e9;
            while replaced < replacements {
                let offset = (u128::from(replaced) * 1_000_000_000 / u128::from(churn_per_s)) as u64;
                let at = start + offset;
                if at > t as u64 { break; }
                wait_for(at);
                let slot = (replaced % sessions) as usize;
                senders[slot].send(Work::Replace(sessions + replaced)).unwrap();
                replaced += 1;
            }
            if t as u64 >= end { break; }
            x = splitmix(x);
            let s = if skew_clients == 0 {
                reserved_hot + x % (sessions - reserved_hot)
            } else if x & 1 == 0 {
                (x >> 1) % skew_clients
            } else {
                skew_clients + (x >> 1) % (sessions - skew_clients)
            };
            wait_for(t as u64);
            let turn = Turn::make(run, id, f, par);
            id += 1;
            issued += 1;
            let _ = senders[s as usize].send(Work::Turn(turn, t as u64));
        }
        drop(senders);
        issued
    });
    let open_issued = disp.join().unwrap();
    let results: Vec<_> = rt.block_on(async {
        let mut v = Vec::new();
        for h in handles { v.push(h.await.unwrap()); }
        v
    });
    let mut lat: Vec<u64> = Vec::new();
    let mut skew_e2e = Vec::new();
    let mut regular_e2e = Vec::new();
    let mut e2e: Vec<u64> = Vec::new();
    let mut max_added_turn: Option<(u64, &Rec)> = None;
    let mut last_reply = 0;
    // Write diagnostics only after all replies and connection closes.
    let mut slow = kv.get("slow_turns").map(|path| {
        let mut out = BufWriter::new(std::fs::File::create(path).unwrap());
        writeln!(out, "client\tintended_ns\tready_ns\tsent_ns\tdone_ns\theld_ns\tadded_ns\tarrival_from_end_ns\tturn").unwrap();
        out
    });
    let (mut completed, mut failures, mut timeouts, mut late) = (0u64, 0u64, 0u64, 0u64);
    let (mut hot_issued, mut hot_completed, mut in_window) = (0u64, 0u64, 0u64);
    for (client, (recs, t, hot_count, _closed)) in results.iter().enumerate() {
        timeouts += t;
        hot_issued += hot_count;
        for r in recs {
            completed += 1;
            last_reply = last_reply.max(r.done);
            if !r.ok { failures += 1; }
            if r.done > end + 1_000_000_000 { late += 1; }
            if r.done >= warm_ns && r.done < end {
                in_window += 1;
            }
            let added = (r.done - r.ready).saturating_sub(r.held);
            if added > 20_000_000 && let Some(out) = &mut slow {
                let arrival_from_end = r.intended as i64 - end as i64;
                writeln!(out, "{client}\t{}\t{}\t{}\t{}\t{}\t{added}\t{arrival_from_end}\t{}",
                         r.intended, r.ready, r.sent, r.done, r.held, r.id).unwrap();
            }
            if *hot_count != 0 {
                if r.ok && r.done < end { hot_completed += 1; }
            } else {
                let turn_e2e = r.done - r.intended;
                e2e.push(turn_e2e);
                if r.intended < warm_ns { continue; }
                lat.push(added);
                if max_added_turn.is_none_or(|(max, _)| added > max) {
                    max_added_turn = Some((added, r));
                }
                if skew_clients != 0 {
                    if (client as u64) < skew_clients {
                        skew_e2e.push(turn_e2e);
                    } else {
                        regular_e2e.push(turn_e2e);
                    }
                }
            }
        }
    }
    if let Some(mut out) = slow { out.flush().unwrap(); }
    lat.sort_unstable();
    e2e.sort_unstable();
    let q = |p: f64| if lat.is_empty() { 0 } else { lat[((lat.len() as f64 * p) as usize).min(lat.len() - 1)] };
    let qe = |p: f64| if e2e.is_empty() { 0 } else { e2e[((e2e.len() as f64 * p) as usize).min(e2e.len() - 1)] };
    let issued = open_issued + hot_issued;
    let hot_turns_per_s = hot_completed as f64 / dur;
    let (max_ready, max_done) = max_added_turn.map_or((0, 0), |(_, r)| (r.ready, r.done));
    let first_close = results.iter().map(|r| r.3).min().unwrap_or(0);
    let p99 = |samples: &mut Vec<u64>| {
        samples.sort_unstable();
        samples.get(samples.len() * 99 / 100).copied().unwrap_or(0)
    };
    let skew_fields = if skew_clients == 0 {
        String::new()
    } else {
        let skew_p99 = p99(&mut skew_e2e);
        let regular_p99 = p99(&mut regular_e2e);
        format!(",\"skew_clients\":{skew_clients},\"skew_e2e_p99_ns\":{skew_p99},\"regular_e2e_p99_ns\":{regular_p99}")
    };
    let detail_fields = if detail {
        format!(",\"first_close_ns\":{first_close},\"last_reply_ns\":{last_reply}")
    } else { String::new() };
    let churn_fields = churn.map_or_else(String::new, |churn| format!(concat!(
        ",\"churn_per_s\":{},\"replacements_scheduled\":{},\"replacements_completed\":{},",
        "\"replacements_after_warm_in_run\":{},\"minimum_live_during_churn\":{}"
    ), churn_per_s, replacements, churn.completed.load(Relaxed),
       churn.completed_in_window.load(Relaxed), churn.minimum_live.load(Relaxed)));
    println!(concat!(
        "{{\"issued\":{},\"completed\":{},\"failures\":{},\"timeouts\":{},\"late\":{},\"rate\":{},",
        "\"measured\":{},\"p50_ns\":{},\"p99_ns\":{},\"p999_ns\":{},\"max_ns\":{},\"e2e_p50_ns\":{},\"e2e_p99_ns\":{},",
        "\"hot_issued\":{},\"hot_completed_in_run\":{},\"hot_turns_per_s\":{},",
        "\"start_ns\":{},\"warm_ns\":{},\"end_ns\":{},\"completed_after_warm_in_run\":{},",
        "\"max_added_ready_ns\":{},\"max_added_done_ns\":{}{}{}{}}}"
    ), issued, completed, failures, timeouts, late, rate,
        lat.len(), q(0.5), q(0.99), q(0.999), lat.last().copied().unwrap_or(0), qe(0.5), qe(0.99),
        hot_issued, hot_completed, hot_turns_per_s, start, warm_ns, end, in_window, max_ready, max_done,
        detail_fields, churn_fields, skew_fields);
}
