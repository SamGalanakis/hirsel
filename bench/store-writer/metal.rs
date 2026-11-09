// The reference uses std::fs::File::write_all and sync_all, the same Rust
// std I/O operations used by Hirsel's local file host adapter. No store codec.
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

struct Counting;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
// SAFETY: every operation forwards its unchanged allocation contract to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let accounts: u64 = args[2].parse()?;
    let initial: u64 = args[3].parse()?;
    let count: u64 = args[4].parse()?;
    let amount: u64 = args[5].parse()?;
    let group: u64 = args[6].parse()?;
    let clients: u64 = args[7].parse()?;
    let raw = args.get(8).is_some_and(|s| s == "raw");
    assert!(accounts >= 2 && count > 0 && group > 0 && clients > 0);
    assert!(count.is_multiple_of(group) && count.is_multiple_of(accounts));
    let mut balances = vec![initial; accounts as usize];
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    // Pin the new name before timing. The hot reference only appends to it.
    std::fs::File::open(std::path::Path::new(path).parent().unwrap())?.sync_all()?;
    let (send, receive) =
        mpsc::sync_channel::<(u64, Instant, mpsc::SyncSender<()>)>(clients as usize);
    let mut latency = Vec::with_capacity((count / group) as usize);
    let mut encoded = Vec::with_capacity((group * 24) as usize);
    println!("READY");
    io::stdout().flush()?;
    io::stdin()
        .lock()
        .lines()
        .next()
        .ok_or("missing start gate")??;
    let allocations = ALLOCATIONS.load(Relaxed);
    let start = Instant::now();
    if raw {
        for _ in 0..count {
            let submitted = Instant::now();
            file.write_all(&[0])?;
            file.sync_all()?;
            latency.push(submitted.elapsed().as_nanos());
        }
    } else {
        thread::scope(|scope| -> io::Result<()> {
            let send = Arc::new(send);
            for id in 0..clients {
                let send = send.clone();
                scope.spawn(move || {
                    let (ack, replies) = mpsc::sync_channel(1);
                    for first in ((id * group)..count).step_by((clients * group) as usize) {
                        send.send((first, Instant::now(), ack.clone())).unwrap();
                        replies.recv().unwrap();
                    }
                });
            }
            for _ in 0..count / group {
                let (first, submitted, reply) = receive.recv().unwrap();
                encoded.clear();
                for offset in 0..group {
                    let from = (first + offset) % accounts;
                    let to = (from + 1) % accounts;
                    let amount = amount * (1 + (first + offset) % 3);
                    assert!(balances[from as usize] >= amount);
                    balances[from as usize] -= amount;
                    balances[to as usize] += amount;
                    for value in [from, to, amount] {
                        encoded.extend_from_slice(&value.to_le_bytes());
                    }
                }
                file.write_all(&encoded)?;
                file.sync_all()?;
                latency.push(submitted.elapsed().as_nanos());
                reply.send(()).unwrap();
            }
            Ok(())
        })?;
    }
    let elapsed = start.elapsed().as_nanos();
    let allocations = ALLOCATIONS.load(Relaxed) - allocations;
    println!("DONE");
    io::stdout().flush()?;
    io::stdin()
        .lock()
        .lines()
        .next()
        .ok_or("missing end gate")??;
    if !raw {
        let mut expected = vec![initial; accounts as usize];
        for index in 0..count {
            let from = index % accounts;
            let to = (from + 1) % accounts;
            let value = amount * (1 + index % 3);
            expected[from as usize] -= value;
            expected[to as usize] += value;
        }
        assert_eq!(balances, expected);
    }
    assert_eq!(balances.iter().sum::<u64>(), initial * accounts);
    println!("elapsed {elapsed}\nallocations {allocations}");
    for value in latency {
        println!("latency {value}");
    }
    println!("verified");
    Ok(())
}
