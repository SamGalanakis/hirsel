// Safe matched I/O baseline, std BufRead::read_line with a reused String / BufWriter.
// Reviewed by the orchestrator, 2026-11-13.
// Same 4096 capacity, same 65536 64-byte lines. Checked arithmetic requires
// the measurement runner: opt3/opt0, overflow-checks=yes, codegen-units=1, no LTO.
use std::io::{BufRead, BufReader, BufWriter, Write};
fn main() {
    if let Some(path) = std::env::args().nth(1) {
        let file = std::fs::File::open(path).unwrap();
        let mut reader = BufReader::with_capacity(4096, file);
        let mut line = String::with_capacity(4096);
        let (mut count, mut length) = (0u64, 0u64);
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 { break; }
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') { line.pop(); }
            }
            count += 1; length += line.len() as u64;
        }
        println!("{count} {length}");
    } else {
        let mut out = BufWriter::with_capacity(4096, std::fs::OpenOptions::new().write(true).open("/dev/stdout").unwrap());
        for _ in 0..65536 {
            out.write_all(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.\n").unwrap();
        }
        out.flush().unwrap();
    }
}
