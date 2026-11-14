//! Idiomatic twin of core/programs/m2b/dispatch `main measure` on both sides: a server and a
//! client exchange 100 requests for `double("ab")` through a spool directory. A request
//! names its handler by the same identity bytes the Hirsel program sends (Rust has no code
//! identity, so the string is written out); the server's registry maps those bytes to a
//! function. A file is published by writing `<name>.tmp` and renaming it into place; the
//! waiting side opens the final name, and on NotFound sleeps 1 ms and tries again, giving up
//! after 10 s of wall time. The frames are the Hirsel ones (core/programs/m2b/dispatch/spool.hirsel).
//!
//! Usage: `m2b-dispatch serve DIR` or `m2b-dispatch client DIR`. Each prints the same three
//! lines as the Hirsel side: the count served (server) or the sum of reply values
//! (client, 100 × 104), then its opens and sleeps.
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

const PERIOD: Duration = Duration::from_millis(1);
const BUDGET: Duration = Duration::from_secs(10);
const DOUBLE: &[u8] =
    b"\x01rpc@1/handlers/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]";

type Handler = fn(&[u8]) -> Vec<u8>;

#[derive(Default)]
struct Stats {
    opens: u64,
    sleeps: u64,
}

enum Reply {
    Done(Vec<u8>),
    NoCode,
    Bad,
}

fn double(b: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(b.len() * 2);
    o.extend_from_slice(b);
    o.extend_from_slice(b);
    o
}

fn put_field(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}

fn request(id: &[u8], arg: &[u8]) -> Vec<u8> {
    let mut out = b"HQ\x01".to_vec();
    put_field(&mut out, id);
    put_field(&mut out, arg);
    out
}

fn field(b: &[u8], at: usize) -> Option<(usize, usize)> {
    let n = u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?);
    let lo = at + 8;
    let hi = lo.checked_add(usize::try_from(n).ok()?)?;
    (hi <= b.len()).then_some((lo, hi))
}

fn dispatch(registry: &HashMap<Vec<u8>, Handler>, frame: &[u8]) -> Reply {
    if !frame.starts_with(b"HQ\x01") {
        return Reply::Bad;
    }
    let Some((id_lo, id_hi)) = field(frame, 3) else {
        return Reply::Bad;
    };
    let Some((arg_lo, arg_hi)) = field(frame, id_hi) else {
        return Reply::Bad;
    };
    if arg_hi != frame.len() {
        return Reply::Bad;
    }
    match registry.get(&frame[id_lo..id_hi]) {
        Some(h) => Reply::Done(h(&frame[arg_lo..arg_hi])),
        None => Reply::NoCode,
    }
}

fn reply_bytes(r: &Reply) -> Vec<u8> {
    let mut out = b"HR\x01".to_vec();
    match r {
        Reply::Done(o) => {
            out.push(0);
            put_field(&mut out, o);
        }
        Reply::NoCode => out.push(1),
        Reply::Bad => out.push(2),
    }
    out
}

fn reply_value(b: &[u8]) -> u64 {
    match b.get(..4) {
        Some([b'H', b'R', 1, 0]) => match field(b, 4) {
            Some((lo, hi)) => 100 + (hi - lo) as u64,
            None => 904,
        },
        Some([b'H', b'R', 1, 1]) if b.len() == 4 => 1,
        Some([b'H', b'R', 1, 2]) if b.len() == 4 => 2,
        _ => 904,
    }
}

fn publish(d: &Path, kind: &str, seq: u64, data: &[u8], s: &mut Stats) {
    let tmp = d.join(format!("{kind}{seq}.tmp"));
    s.opens += 1;
    fs::write(&tmp, data).expect("write");
    fs::rename(&tmp, d.join(format!("{kind}{seq}"))).expect("rename");
}

fn take_file(d: &Path, kind: &str, seq: u64, s: &mut Stats) -> Option<Vec<u8>> {
    let path = d.join(format!("{kind}{seq}"));
    let start = Instant::now();
    loop {
        s.opens += 1;
        match fs::read(&path) {
            Ok(b) => {
                fs::remove_file(&path).expect("remove");
                return Some(b);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => panic!("read: {e}"),
        }
        if start.elapsed() >= BUDGET {
            return None;
        }
        s.sleeps += 1;
        sleep(PERIOD);
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let (Some(role), Some(dir)) = (argv.get(1), argv.get(2)) else {
        eprintln!("usage: m2b-dispatch serve|client DIR");
        std::process::exit(2);
    };
    let d = Path::new(dir);
    let mut s = Stats::default();
    let first = match role.as_str() {
        "serve" => {
            let mut registry: HashMap<Vec<u8>, Handler> = HashMap::new();
            registry.insert(DOUBLE.to_vec(), double);
            let mut served = 0;
            for seq in 0..100 {
                let Some(frame) = take_file(d, "q", seq, &mut s) else {
                    break;
                };
                publish(
                    d,
                    "r",
                    seq,
                    &reply_bytes(&dispatch(&registry, &frame)),
                    &mut s,
                );
                served += 1;
            }
            served
        }
        "client" => (0..100)
            .map(|seq| {
                publish(d, "q", seq, &request(DOUBLE, b"ab"), &mut s);
                take_file(d, "r", seq, &mut s).map_or(903, |b| reply_value(&b))
            })
            .sum(),
        _ => std::process::exit(2),
    };
    println!("{first}\n{}\n{}", s.opens, s.sleeps);
}
