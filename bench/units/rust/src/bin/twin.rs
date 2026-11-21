//! Part 3 Tokio twin of bench/units/session_u.hirsel (S1 shape): sessions,
//! P partition committers with group commit, and the shared service are
//! tasks; nothing runs on the main thread (it only waits). The partition
//! write runs on the committer's worker; fdatasync runs on one of `syncs`
//! dedicated threads, as HIRSEL_SYNC_HELPERS does for hirsel.
//! Usage: twin PORT SESSIONS PARTITIONS PAR TOOL_PORT WORKERS SYNCS DIR [READY_ACK]
//!        twin synccost DIR N SIZE   (cost of write+fdatasync alone)
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, mpsc as smpsc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use units_bench::*;

type Commit = (Vec<u8>, oneshot::Sender<u64>);
type SvcReq = (u64, oneshot::Sender<u64>);
type SyncJob = (i32, oneshot::Sender<i32>);

fn sync_pool(n: usize) -> smpsc::Sender<SyncJob> {
    let (tx, rx) = smpsc::channel::<SyncJob>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..n {
        let rx = rx.clone();
        std::thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            let Ok((fd, done)) = job else { return };
            let r = unsafe { libc::fdatasync(fd) };
            let _ = done.send(r);
        });
    }
    tx
}

async fn committer(mut file: std::fs::File, mut rx: mpsc::Receiver<Commit>, sync: smpsc::Sender<SyncJob>) -> u64 {
    let mut groups = 0u64;
    let mut batch = Vec::with_capacity(65536);
    let mut acks = Vec::new();
    while let Some((rec, ack)) = rx.recv().await {
        batch.clear();
        batch.extend_from_slice(&rec);
        acks.push(ack);
        while let Ok((rec, ack)) = rx.try_recv() {
            batch.extend_from_slice(&rec);
            acks.push(ack);
        }
        file.write_all(&batch).unwrap();
        let (tx, done) = oneshot::channel();
        sync.send((file.as_raw_fd(), tx)).unwrap();
        assert_eq!(done.await.unwrap(), 0, "fdatasync");
        groups += 1;
        for a in acks.drain(..) { let _ = a.send(groups); }
    }
    groups
}

async fn service(mut rx: mpsc::Receiver<SvcReq>) -> u64 {
    let mut calls = 0u64;
    while let Some((x, reply)) = rx.recv().await {
        calls += 1;
        let _ = reply.send(kernel(x, SVC_ROUNDS));
    }
    calls
}

struct Tool { s: TcpStream, buf: Vec<u8> }

async fn tool_call(t: &mut Tool, size: usize, seed: u64) -> (u64, u64) {
    let mut req = [0u8; 16];
    req[0..8].copy_from_slice(&(size as u64).to_le_bytes());
    req[8..16].copy_from_slice(&seed.to_le_bytes());
    t.s.write_all(&req).await.unwrap();
    t.buf.resize(8 + size, 0);
    t.s.read_exact(&mut t.buf).await.unwrap();
    let held = u64::from_le_bytes(t.buf[0..8].try_into().unwrap());
    (first_word(&t.buf[8..]), held)
}

async fn open_tool(port: u16) -> Tool {
    let s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.set_nodelay(true).unwrap();
    Tool { s, buf: Vec::with_capacity(8 + 65536) }
}

async fn session(mut st: TcpStream, commit: mpsc::Sender<Commit>, svc: mpsc::Sender<SvcReq>, tool_port: u16, par: bool, ready_sid: Option<u64>) -> u64 {
    let mut tools = Vec::new();
    for _ in 0..if par { 16 } else { 1 } { tools.push(open_tool(tool_port).await); }
    if let Some(sid) = ready_sid {
        st.write_all(&sid.to_le_bytes()).await.unwrap();
    }
    let mut req = [0u8; REQ];
    let mut turns = 0u64;
    while st.read_exact(&mut req).await.is_ok() {
        let turn = Turn::decode(&req);
        let (mut acc, mut held) = (0u64, 0u64);
        if turn.par == 0 {
            let t = &mut tools[0];
            for i in 0..turn.ncalls as u64 {
                let (d, h) = tool_call(t, call_size(turn.codes[i as usize]), turn.seed.wrapping_add(i)).await;
                acc ^= d;
                held += h;
            }
        } else {
            let mut jobs = Vec::new();
            for i in 0..turn.ncalls as u64 {
                let mut t = tools.pop().unwrap();
                let size = call_size(turn.codes[i as usize]);
                let seed = turn.seed.wrapping_add(i);
                jobs.push(tokio::spawn(async move { let r = tool_call(&mut t, size, seed).await; (t, r) }));
            }
            for j in jobs.into_iter().rev() {
                let (t, (d, h)) = j.await.unwrap();
                tools.push(t);
                acc ^= d;
                held = held.max(h);
            }
        }
        if turn.shared != 0 {
            let (tx, rx) = oneshot::channel();
            svc.send((turn.id ^ turn.seed, tx)).await.unwrap();
            acc ^= rx.await.unwrap();
        }
        let mut rec = Vec::with_capacity(64);
        rec.extend_from_slice(&turn.id.to_le_bytes());
        rec.extend_from_slice(&acc.to_le_bytes());
        rec.extend_from_slice(&held.to_le_bytes());
        rec.resize(64, 0);
        let (tx, rx) = oneshot::channel();
        commit.send((rec, tx)).await.unwrap();
        rx.await.unwrap();
        let mut reply = [0u8; REPLY];
        reply[0..8].copy_from_slice(&turn.id.to_le_bytes());
        reply[8..16].copy_from_slice(&acc.to_le_bytes());
        reply[16..24].copy_from_slice(&held.to_le_bytes());
        st.write_all(&reply).await.unwrap();
        turns += 1;
    }
    turns
}

fn synccost(a: &[String]) {
    let dir = std::path::Path::new(&a[2]);
    let n: usize = a[3].parse().unwrap();
    let size: usize = a[4].parse().unwrap();
    let mut f = std::fs::File::create(dir.join("synccost.log")).unwrap();
    let rec = vec![7u8; size];
    let mut v = Vec::new();
    for _ in 0..n {
        let t0 = now_ns();
        f.write_all(&rec).unwrap();
        assert_eq!(unsafe { libc::fdatasync(f.as_raw_fd()) }, 0);
        v.push(now_ns() - t0);
    }
    v.sort_unstable();
    println!("{{\"synccost_size\":{size},\"n\":{n},\"p50_ns\":{},\"p99_ns\":{},\"max_ns\":{}}}", v[n / 2], v[n * 99 / 100], v[n - 1]);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a[1] == "synccost" { return synccost(&a); }
    let port: u16 = a[1].parse().unwrap();
    let sessions: u64 = a[2].parse().unwrap();
    let partitions: usize = a[3].parse().unwrap();
    let par = a[4] == "1";
    let tool_port: u16 = a[5].parse().unwrap();
    let workers: usize = a[6].parse().unwrap();
    let syncs: usize = a[7].parse().unwrap();
    let dir = std::path::PathBuf::from(&a[8]);
    let ready_ack = a.get(9).is_some_and(|arg| arg == "1");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(workers).enable_all().build().unwrap();
    let main = rt.spawn(async move {
        let sync = sync_pool(syncs);
        let mut commits = Vec::new();
        let mut committers = Vec::new();
        for p in 0..partitions {
            let file = std::fs::File::create(dir.join(format!("p{p}.log"))).unwrap();
            let (tx, rx) = mpsc::channel(4096);
            commits.push(tx);
            committers.push(tokio::spawn(committer(file, rx, sync.clone())));
        }
        let (svc_tx, svc_rx) = mpsc::channel(1024);
        let svc = tokio::spawn(service(svc_rx));
        let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let mut jobs = Vec::new();
        while jobs.len() < sessions as usize {
            let (mut st, _) = l.accept().await.unwrap();
            st.set_nodelay(true).unwrap();
            let mut hello = [0u8; 8];
            if st.read_exact(&mut hello).await.is_err() { continue; }
            let sid = u64::from_le_bytes(hello);
            if sid >= sessions { continue; }
            let commit = commits[(sid % partitions as u64) as usize].clone();
            jobs.push(tokio::spawn(session(st, commit, svc_tx.clone(), tool_port, par, ready_ack.then_some(sid))));
        }
        let mut turns = 0;
        for j in jobs { turns += j.await.unwrap(); }
        drop(commits);
        drop(svc_tx);
        let mut groups = 0;
        for c in committers { groups += c.await.unwrap(); }
        let calls = svc.await.unwrap();
        eprintln!("twin turns={turns} groups={groups} svc={calls}");
    });
    rt.block_on(main).unwrap();
}
