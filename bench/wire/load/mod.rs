use crate::client::{Client, Socket, returned};
use crate::codec::{self, Frame, Want};
use crate::{BITS, CHUNKS, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy)]
pub struct Config {
    pub clients: usize,
    pub rate: u32,
    pub seconds: u32,
}
struct Job {
    p: u64,
    bit: u32,
    scheduled: Instant,
}
enum Event {
    Sent {
        owner: usize,
        p: u64,
        scheduled: Instant,
        socket: Instant,
    },
    Returned {
        owner: usize,
        p: u64,
        version: u64,
    },
    Observed {
        version: u64,
        chunks: HashSet<u32>,
        at: Instant,
    },
}
struct Sample {
    scheduled: Instant,
    socket: Instant,
    version: Option<u64>,
}

pub fn distribution(samples: &mut [u64]) -> Value {
    samples.sort_unstable();
    if samples.is_empty() {
        return json!({"count": 0, "p50_us": null, "p99_us": null});
    }
    let at = |q: usize| samples[(samples.len() * q).div_ceil(100).saturating_sub(1)];
    json!({"count": samples.len(), "p50_us": at(50) as f64 / 1000.0, "p99_us": at(99) as f64 / 1000.0})
}
fn ns(d: Duration) -> u64 {
    d.as_nanos().try_into().unwrap_or(u64::MAX)
}
fn cpu_seconds() -> Result<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let rest: Vec<_> = stat
        .rsplit_once(')')
        .ok_or("invalid proc stat")?
        .1
        .split_whitespace()
        .collect();
    // Linux user HZ is 100 on the benchmark host, as reported by sysconf.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    Ok((rest[11].parse::<u64>()? + rest[12].parse::<u64>()?) as f64 / ticks as f64)
}

/// One observer and N-1 calling connections, all subscribed. The observer is always
/// a different socket from the caller; all timestamps use this process's clock.
pub async fn run(url: &str, wants: &[Want], config: Config) -> Result<Value> {
    if config.clients < 2 || config.rate == 0 || config.seconds == 0 {
        return Err("load requires >=2 clients and positive rate/seconds".into());
    }
    let offered = u64::from(config.rate) * u64::from(config.seconds);
    let mut clients = Vec::with_capacity(config.clients);
    for _ in 0..config.clients {
        match connect_subscribed(url, wants).await {
            Ok(client) => clients.push(client),
            Err(error) => {
                return Ok(json!({
                    "complete": false, "connections": clients.len(), "requested_connections": config.clients,
                    "setup_error": error.to_string(), "planned_offers": offered,
                    "rate_per_second": config.rate, "seconds": config.seconds,
                    "offered": 0, "admitted": 0, "returned": 0, "observed": 0,
                    "calls_sharing_observer_batch": 0, "schedule_lag": distribution(&mut []),
                    "socket_to_observe": distribution(&mut []), "scheduled_to_observe": distribution(&mut []),
                }));
            }
        }
    }
    let observer = clients.remove(0);
    let (events, event_rx) = mpsc::channel(8192);
    let collector = tokio::spawn(collect(event_rx));
    let (stop_tx, stop_rx) = oneshot::channel();
    let observe = tokio::spawn(observe(observer, events.clone(), stop_rx));
    let mut tasks = JoinSet::new();
    let mut arrivals = Vec::new();
    for (owner, client) in clients.into_iter().enumerate() {
        let (tx, rx) = mpsc::channel(1);
        arrivals.push(tx);
        tasks.spawn(caller(owner, client, rx, events.clone()));
    }
    let start = Instant::now() + Duration::from_millis(20);
    let cpu = cpu_seconds()?;
    let wall = Instant::now();
    let mut missed = 0u64;
    let mut positions = vec![0u64; arrivals.len()];
    for i in 0..offered {
        let scheduled = start + Duration::from_secs_f64(i as f64 / f64::from(config.rate));
        sleep_until(scheduled).await;
        let owner = i as usize % arrivals.len();
        let p = positions[owner];
        let job = Job {
            p,
            bit: (i % u64::from(BITS)) as u32,
            scheduled,
        };
        match arrivals[owner].try_send(job) {
            Ok(()) => positions[owner] += 1,
            Err(mpsc::error::TrySendError::Full(_)) => missed += 1,
            // A caller ends only on an error; joining below reports that error.
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
    drop(arrivals);
    let mut run_error = None;
    while let Some(joined) = tasks.join_next().await {
        let outcome = joined
            .map_err(crate::Error::from)
            .and_then(|outcome| outcome);
        run_error = run_error.or_else(|| outcome.err());
    }
    if run_error.is_none() {
        // A snapshot gives a final-version barrier without changing the observer's cache.
        let barrier = async {
            let mut barrier = connect_subscribed(url, wants).await?;
            let final_version = barrier.version;
            barrier.socket.close(None).await?;
            Ok::<_, crate::Error>(final_version)
        }
        .await;
        match barrier {
            Ok(final_version) => {
                let _ = stop_tx.send(final_version);
            }
            Err(error) => {
                run_error = Some(error);
                drop(stop_tx);
            }
        }
    } else {
        drop(stop_tx);
    }
    let observation = observe
        .await
        .map_err(crate::Error::from)
        .and_then(|outcome| outcome);
    run_error = run_error.or_else(|| observation.err());
    drop(events);
    let mut result = collector.await??;
    let cpu = cpu_seconds()? - cpu;
    let elapsed = wall.elapsed().as_secs_f64();
    result["connections"] = json!(config.clients);
    result["offered"] = json!(offered);
    result["rate_per_second"] = json!(config.rate);
    result["seconds"] = json!(config.seconds);
    result["missed_arrivals"] = json!(missed);
    result["wall_seconds"] = json!(elapsed);
    result["generator_cpu_seconds"] = json!(cpu);
    result["generator_cpu_us_per_offered"] = json!(cpu * 1e6 / offered as f64);
    result["complete"] =
        json!(run_error.is_none() && missed == 0 && result["observed"].as_u64() == Some(offered));
    result["run_error"] = json!(run_error.map(|error| error.to_string()));
    result["clock"] = json!(
        "one process monotonic clock; scheduled arrival and immediately-before-socket-send timestamps"
    );
    Ok(result)
}

async fn connect_subscribed(url: &str, wants: &[Want]) -> Result<Client> {
    let mut client = Client::connect(url, wants, None).await?;
    client.snapshot().await?;
    Ok(client)
}

/// Seed one row per chunk, then keep the requested number of sockets live
/// until the server's settled RSS has been read.
pub async fn memory(
    url: &str,
    wants: &[Want],
    count: usize,
    subscribed: bool,
    pid: u32,
    snapshots: Option<&std::path::Path>,
) -> Result<Value> {
    let wall = Instant::now();
    // At most 32 calls per principal, below the default burst of 40.
    for chunks in (0..CHUNKS).step_by(32) {
        let mut writer = Client::connect(url, wants, None).await?;
        for (p, chunk) in (chunks..(chunks + 32).min(CHUNKS)).enumerate() {
            writer.start_call(0, p as u64, chunk << 9).await?;
            writer.finish_call(0, p as u64).await?;
        }
        writer.socket.close(None).await?;
    }
    let mut witness = Client::connect(url, wants, None).await?;
    let rows = witness.snapshot().await?;
    if rows != CHUNKS as usize || !witness.image.iter().step_by(64).all(|byte| *byte == 1) {
        return Err(format!(
            "seed witness expected {CHUNKS} chunks and each first bit set, found {rows} rows"
        )
        .into());
    }
    let version = witness.version;
    witness.socket.close(None).await?;
    let idle_sockets = if let Some(out) = snapshots {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Some(memory_snapshot(out, "seeded-idle", pid)?)
    } else {
        None
    };
    let mut sockets = JoinSet::new();
    let mut setup_error = None;
    for _ in 0..count {
        let result = async {
            let mut client = Client::connect(url, wants, None).await?;
            if subscribed && (client.snapshot().await? != rows || client.image != witness.image) {
                return Err("subscribed session received an incomplete snapshot".into());
            }
            Ok::<_, crate::Error>(client.socket)
        }
        .await;
        match result {
            Ok(socket) => {
                sockets.spawn(hold_idle(socket));
            }
            Err(error) => {
                setup_error = Some(error.to_string());
                break;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    while let Some(closed) = sockets.try_join_next() {
        setup_error.get_or_insert_with(|| closed.unwrap_or_else(|error| error.to_string()));
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))?;
    let field = |text: &str, name: &str| -> Result<u64> {
        Ok(text
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .ok_or_else(|| format!("missing process metric {name}"))?
            .parse()?)
    };
    let mut result = json!({
        "complete": setup_error.is_none() && sockets.len() == count,
        "connections": sockets.len(), "requested_connections": count, "subscribed": subscribed,
        "setup_error": setup_error, "seeded_chunks": rows, "seed_version": version,
        "server_rss_bytes": field(&smaps, "Rss:")? * 1024,
        "server_virtual_bytes": field(&status, "VmSize:")? * 1024,
        "server_peak_rss_bytes": field(&status, "VmHWM:")? * 1024,
        "server_threads": field(&status, "Threads:")?,
        "wall_seconds": wall.elapsed().as_secs_f64(),
    });
    if let Some(out) = snapshots {
        let live_sockets = memory_snapshot(out, "held", pid)?;
        sockets.shutdown().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let after_sockets = memory_snapshot(out, "after-close", pid)?;
        result["socket_counts"] = json!({
            "seeded_idle": idle_sockets.as_ref().map(HashSet::len),
            "held": live_sockets.len(), "after_close": after_sockets.len(),
        });
        let drained = idle_sockets.as_ref() == Some(&after_sockets);
        result["drained"] = json!(drained);
        if !drained {
            result["complete"] = json!(false);
        }
    }
    Ok(result)
}

fn memory_snapshot(
    out: &std::path::Path,
    name: &str,
    pid: u32,
) -> Result<HashSet<std::path::PathBuf>> {
    let output = out.join(name);
    std::fs::create_dir_all(&output)?;
    let process = std::path::PathBuf::from(format!("/proc/{pid}"));
    for file in ["smaps", "smaps_rollup", "status"] {
        std::fs::write(output.join(file), std::fs::read(process.join(file))?)?;
    }
    let mut sockets = HashSet::new();
    for entry in std::fs::read_dir(process.join("fd"))? {
        match entry?.path().read_link() {
            Ok(target)
                if target
                    .as_os_str()
                    .as_encoded_bytes()
                    .starts_with(b"socket:[") =>
            {
                sockets.insert(target);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(sockets)
}

// Service ping/pong while a socket waits for the memory sample.
async fn hold_idle(mut socket: Socket) -> String {
    while let Some(message) = socket.next().await {
        match message {
            Ok(Message::Ping(_)) => {
                // Tungstenite queues the Pong on read.
                if let Err(error) = socket.flush().await {
                    return error.to_string();
                }
            }
            Ok(Message::Close(frame)) => return format!("idle socket closed: {frame:?}"),
            Err(error) => return error.to_string(),
            _ => (),
        }
    }
    String::from("idle socket reached EOF")
}

async fn caller(
    owner: usize,
    mut client: Client,
    mut jobs: mpsc::Receiver<Job>,
    events: mpsc::Sender<Event>,
) -> Result<()> {
    let mut inflight = None;
    loop {
        tokio::select! {
            job = jobs.recv(), if inflight.is_none() => {
                let Some(job) = job else { client.socket.close(None).await?; return Ok(()); };
                let socket = client.start_call_timed(0, job.p, job.bit).await?;
                events.send(Event::Sent { owner, p: job.p, scheduled: job.scheduled, socket }).await?;
                inflight = Some(job.p);
            }
            frame = client.receive() => {
                let frame = frame?;
                let (version, done) = match frame { Frame::Batch(v, _, _, d) | Frame::Reset(v, d) => (v, d), _ => continue };
                for d in done {
                    if d.stream != 0 || inflight != Some(d.p) { return Err("load saw an unexpected final outcome".into()); }
                    returned(&d)?;
                    events.send(Event::Returned { owner, p: d.p, version }).await?; inflight = None;
                }
            }
        }
    }
}
async fn observe(
    mut observer: Client,
    events: mpsc::Sender<Event>,
    mut stop: oneshot::Receiver<u64>,
) -> Result<()> {
    let mut final_version = None;
    loop {
        if final_version.is_some_and(|v| observer.version >= v) {
            observer.socket.close(None).await?;
            return Ok(());
        }
        tokio::select! {
            v = &mut stop, if final_version.is_none() => final_version = Some(v?),
            frame = observer.receive_timed() => {
                let (at, frame) = frame?;
                match frame {
                    Frame::Batch(version, _, rows, _) => {
                        let chunks = rows.iter().map(|r| u32::from_be_bytes(r.key[4..8].try_into().unwrap())).collect();
                        events.send(Event::Observed { version, chunks, at }).await?;
                    }
                    Frame::Reset(..) => return Err("measurement observer overran; run is incomplete".into()),
                    _ => (),
                }
            }
        }
    }
}

async fn collect(mut events: mpsc::Receiver<Event>) -> Result<Value> {
    let mut pending: HashMap<(usize, u64), Sample> = HashMap::new();
    let mut observed: BTreeMap<u64, Instant> = BTreeMap::new();
    let mut scheduled = Vec::new();
    let mut socket = Vec::new();
    let mut lag = Vec::new();
    let mut returned_count = 0u64;
    let mut admitted = 0u64;
    let mut batches = 0u64;
    let mut rows = 0usize;
    let mut sampled_batches = HashSet::new();
    while let Some(event) = events.recv().await {
        match event {
            Event::Sent {
                owner,
                p,
                scheduled,
                socket,
            } => {
                admitted += 1;
                lag.push(ns(socket.saturating_duration_since(scheduled)));
                if pending
                    .insert(
                        (owner, p),
                        Sample {
                            scheduled,
                            socket,
                            version: None,
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate scheduled call identity".into());
                }
            }
            Event::Returned { owner, p, version } => {
                pending
                    .get_mut(&(owner, p))
                    .ok_or("outcome precedes Sent event")?
                    .version = Some(version);
                returned_count += 1;
            }
            Event::Observed {
                version,
                chunks,
                at,
            } => {
                batches += 1;
                rows += chunks.len();
                observed.entry(version).or_insert(at);
            }
        }
        if pending.values().any(|s| {
            s.version
                .and_then(|v| observed.range(v..).next())
                .is_some_and(|(_, at)| *at < s.socket)
        }) {
            return Err("observer arrival precedes its correlated send".into());
        }
        pending.retain(|_, sample| {
            let Some(version) = sample.version else {
                return true;
            };
            let hit = observed.range(version..).next();
            if let Some((observed_version, at)) = hit {
                sampled_batches.insert(*observed_version);
                scheduled.push(ns(at.saturating_duration_since(sample.scheduled)));
                socket.push(ns(at.duration_since(sample.socket)));
                false
            } else {
                true
            }
        });
        // Keep first arrivals for this finite run. Another caller's Sent event
        // can reach this queue after the observer and after a later outcome.
    }
    Ok(
        json!({ "admitted": admitted, "returned": returned_count, "observed": scheduled.len(), "unobserved": pending.len(),
        "observer_batches": batches, "observer_chunk_rows": rows,
        "calls_sharing_observer_batch": scheduled.len().saturating_sub(sampled_batches.len()),
        "scheduled_to_observe": distribution(&mut scheduled), "socket_to_observe": distribution(&mut socket),
        "schedule_lag": distribution(&mut lag) }),
    )
}

/// WebSocket echo includes framing, sockets, scheduling and this clock. It excludes
/// the WAL and broadcast, giving the generator's loopback timing floor.
pub async fn echo(samples: usize) -> Result<Value> {
    if samples == 0 {
        return Err("echo needs samples".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        socket.set_nodelay(true)?;
        let mut ws =
            tokio_tungstenite::accept_async_with_config(socket, Some(crate::websocket_config()))
                .await?;
        while let Some(message) = ws.next().await {
            match message? {
                Message::Binary(b) => ws.send(Message::Binary(b)).await?,
                Message::Close(_) => break,
                _ => (),
            }
        }
        Ok::<_, crate::Error>(())
    });
    let (mut ws, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/echo"),
        Some(crate::websocket_config()),
        true,
    )
    .await?;
    if let tokio_tungstenite::MaybeTlsStream::Plain(socket) = ws.get_ref() {
        socket.set_nodelay(true)?;
    }
    let payload = codec::encode(&Frame::Call(0, 1, 0, 30_000, 0u32.to_le_bytes().to_vec()));
    let start = Instant::now();
    let cpu = cpu_seconds()?;
    let mut timings = Vec::new();
    for _ in 0..samples {
        let sent = Instant::now();
        ws.send(Message::Binary(payload.clone().into())).await?;
        let reply = ws.next().await.ok_or("echo closed")??;
        if reply != Message::Binary(payload.clone().into()) {
            return Err("echo payload differs".into());
        }
        timings.push(ns(sent.elapsed()));
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cpu = cpu_seconds()? - cpu;
    ws.close(None).await?;
    server.await??;
    Ok(
        json!({"echo_rtt": distribution(&mut timings), "wall_seconds": elapsed,
        "generator_and_echo_cpu_seconds": cpu, "cpu_us_per_echo": cpu * 1e6 / samples as f64}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn idle_socket_answers_ping_and_reports_close() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await?;
            let mut socket = tokio_tungstenite::accept_async(tcp).await?;
            let payload = vec![1, 2, 3];
            socket.send(Message::Ping(payload.clone().into())).await?;
            let pong = socket.next().await.ok_or("socket ended before Pong")??;
            assert_eq!(pong, Message::Pong(payload.into()));
            socket.close(None).await?;
            Ok::<(), crate::Error>(())
        });
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}")).await?;
        let ended = tokio::time::timeout(Duration::from_secs(2), hold_idle(socket)).await?;
        peer.await??;
        assert!(ended.starts_with("idle socket closed"));
        Ok(())
    }

    #[tokio::test]
    async fn observer_timestamp_survives_a_late_sent_notification() -> Result<()> {
        let (tx, rx) = mpsc::channel(8);
        let start = Instant::now();
        tx.send(Event::Sent {
            owner: 0,
            p: 0,
            scheduled: start,
            socket: start,
        })
        .await?;
        tx.send(Event::Observed {
            version: 1,
            chunks: HashSet::new(),
            at: start + Duration::from_micros(1000),
        })
        .await?;
        tx.send(Event::Returned {
            owner: 0,
            p: 0,
            version: 2,
        })
        .await?;
        tx.send(Event::Sent {
            owner: 1,
            p: 0,
            scheduled: start,
            socket: start + Duration::from_micros(500),
        })
        .await?;
        tx.send(Event::Returned {
            owner: 1,
            p: 0,
            version: 1,
        })
        .await?;
        tx.send(Event::Observed {
            version: 2,
            chunks: HashSet::new(),
            at: start + Duration::from_micros(2000),
        })
        .await?;
        drop(tx);
        let result = collect(rx).await?;
        assert_eq!(result["socket_to_observe"]["p50_us"], 500.0);
        assert_eq!(result["scheduled_to_observe"]["p50_us"], 1000.0);
        assert_eq!(result["observed"], 2);
        Ok(())
    }
    #[tokio::test]
    async fn latency_counts_queue_delay_and_waits_for_a_different_observer() -> Result<()> {
        let (tx, rx) = mpsc::channel(8);
        let start = Instant::now();
        tx.send(Event::Sent {
            owner: 1,
            p: 1,
            scheduled: start,
            socket: start + Duration::from_millis(10),
        })
        .await?;
        tx.send(Event::Observed {
            version: 2,
            chunks: HashSet::from([1]),
            at: start + Duration::from_millis(15),
        })
        .await?;
        tx.send(Event::Returned {
            owner: 1,
            p: 1,
            version: 2,
        })
        .await?;
        drop(tx);
        let result = collect(rx).await?;
        assert_eq!(result["scheduled_to_observe"]["p50_us"], 15000.0);
        assert_eq!(result["socket_to_observe"]["p50_us"], 5000.0);
        assert_eq!(result["schedule_lag"]["p50_us"], 10000.0);
        assert_eq!(result["observed"], 1);
        let (tx, rx) = mpsc::channel(8);
        tx.send(Event::Sent {
            owner: 1,
            p: 2,
            scheduled: start,
            socket: start + Duration::from_millis(10),
        })
        .await?;
        tx.send(Event::Observed {
            version: 3,
            chunks: HashSet::new(),
            at: start + Duration::from_millis(5),
        })
        .await?;
        tx.send(Event::Returned {
            owner: 1,
            p: 2,
            version: 3,
        })
        .await?;
        drop(tx);
        assert!(collect(rx).await.is_err());
        Ok(())
    }
}
