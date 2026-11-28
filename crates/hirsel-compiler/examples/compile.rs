//! Compile one package directory through the supervisor and print how it ended:
//! `compile <hirsel> <scratch> <package> <target> <cpu seconds> <memory bytes> <wall ms>`.

use hirsel_compiler::{Caps, Request, Service};
use hirsel_core_layout::Target;
use hirsel_mod::Backend;
use std::time::Instant;

hirsel_guard::install!();

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [
        _,
        worker,
        scratch,
        package,
        target,
        cpu,
        memory,
        wall,
        exports @ ..,
    ] = &args[..]
    else {
        eprintln!(
            "usage: compile <hirsel> <scratch> <package> <target> <cpu> <memory> <wall> [fully qualified export ...]"
        );
        std::process::exit(2);
    };
    let caps = Caps {
        cpu_seconds: cpu.parse().unwrap(),
        memory_bytes: memory.parse().unwrap(),
        wall_ms: wall.parse().unwrap(),
        ..Caps::HOST
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(package).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let name = entry.file_name().into_string().unwrap();
            files.push((name, std::fs::read(entry.path()).unwrap()));
        }
    }
    let files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (&name[..], &bytes[..]))
        .collect();
    let target = Target::from_triple(target).unwrap();
    let service = Service::new(worker.into(), scratch.into(), caps, 1, 0);
    let exports = exports.iter().map(String::as_str).collect::<Vec<_>>();
    let request = Request {
        files: &files,
        target,
        backend: Backend::Cranelift,
        policy: 0,
    };
    let started = Instant::now();
    let answer = service.compile_exports(&request, &exports);
    let took = started.elapsed();
    match answer {
        Ok(built) => println!(
            "ok {} bytes, {} entries in {took:?}",
            built.bytes.len(),
            hirsel_mod::Module::parse(&built.bytes)
                .unwrap()
                .entries()
                .count()
        ),
        Err(refusal) => println!("{refusal:?} in {took:?}"),
    }
}
