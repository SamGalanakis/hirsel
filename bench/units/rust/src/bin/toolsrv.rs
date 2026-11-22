//! Part 3 tool server: a separate process. Each request is size u64, seed
//! u64; it holds the call for the programmed delay and replies with the ns it
//! held (u64) followed by `size` bytes of the fixed tool data.
//! Usage: toolsrv PORT DELAY_US THREADS [PRECISE] (pin with taskset).
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use units_bench::*;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let port: u16 = a[1].parse().unwrap();
    let delay = std::time::Duration::from_micros(a[2].parse().unwrap());
    let threads: usize = a[3].parse().unwrap();
    let precise = a.get(4).is_some_and(|arg| arg == "1");
    let data = Arc::new(tool_data());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    rt.block_on(async move {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        eprintln!("toolsrv ready port={port}");
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            s.set_nodelay(true).unwrap();
            let data = data.clone();
            tokio::spawn(async move {
                let timer = precise.then(|| DeadlineTimer::new().unwrap());
                let mut req = [0u8; 16];
                let mut out = Vec::with_capacity(8 + 65536);
                while s.read_exact(&mut req).await.is_ok() {
                    let size = u64::from_le_bytes(req[0..8].try_into().unwrap()) as usize;
                    if ![1024, 8192, 65536].contains(&size) { break; }
                    let seed = u64::from_le_bytes(req[8..16].try_into().unwrap());
                    let t0 = now_ns();
                    if let Some(timer) = &timer {
                        timer.sleep(delay).await.unwrap();
                    } else {
                        tokio::time::sleep(delay).await;
                    }
                    out.clear();
                    let o = offset(seed);
                    out.extend_from_slice(&(now_ns() - t0).to_le_bytes());
                    out.extend_from_slice(&data[o..o + size]);
                    if s.write_all(&out).await.is_err() { break; }
                }
            });
        }
    });
}
