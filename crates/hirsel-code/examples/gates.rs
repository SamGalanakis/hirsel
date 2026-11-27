//! The loader's G3 and G1c measurements: `gates [module]...`.
//!
//! Loads distinct 8 KiB modules of 200 fixups, each published at once, then swaps one
//! slot while sixteen workers dispatch through it. Each named `--emit module` file is
//! loaded too, its rows bound to a stub that is never called.
//!
//! `gates density <text-align>` instead loads 50,000 distinct 256-byte modules in
//! batches, then lets a growing share of them die, and prints the code band's mappings
//! and the resident memory at each step. `gates releases` measures `activate` while
//! every swap's old code dies and is reclaimed.

use hirsel_code::{Code, Config, Loader, Signature, TARGET};
use hirsel_core_ids::ContentHash;
use hirsel_mod::{
    Backend, Class, Entry, Fixup, Header, Image, Kind, Mask, Module, NO_DESCRIPTOR, Span,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

hirsel_guard::install!();

#[derive(Clone, Copy)]
#[repr(transparent)]
struct Leaf(unsafe extern "C" fn() -> u64);
// SAFETY: the measured modules' one entry takes nothing and returns a word.
unsafe impl Signature for Leaf {
    const DIGEST: ContentHash = ContentHash([9; 32]);
}

const IDENTITY: ContentHash = ContentHash([0xaa; 32]);
const TEXT: usize = 8 << 10;
const FIXUPS: u32 = 200;

extern "C" fn stub() {}

/// `leaf() = n`, padded to 8 KiB, with a data table of 200 text addresses.
fn module(n: u32) -> Vec<u8> {
    shaped(n, TEXT, 64, FIXUPS)
}

/// `leaf() = n` in `length` bytes of text at `align`, with `fixups` data words.
fn shaped(n: u32, length: usize, align: u32, fixups: u32) -> Vec<u8> {
    let mut text = match TARGET.arch() {
        // mov eax, n; ret
        hirsel_core_layout::Arch::X86_64 => [&[0xb8][..], &n.to_le_bytes(), &[0xc3]].concat(),
        // movz w0, n; ret
        hirsel_core_layout::Arch::Aarch64 => [0x5280_0000 | (n & 0xffff) << 5, 0xd65f_03c0u32]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect(),
    };
    text.resize(length, 0);
    Image {
        header: Header {
            target: TARGET,
            backend: Backend::Cranelift,
            policy: 0,
            abi: hirsel_mod::abi(),
            compiler: ContentHash([2; 32]),
            coreir: ContentHash(std::array::from_fn(|at| n.to_le_bytes()[at % 4])),
            text_align: align,
            data_align: 64,
        },
        spans: vec![Span {
            start: 0,
            length: length as u32,
        }],
        names: vec!["leaf".into()],
        text,
        data: vec![0; 8 * fixups as usize],
        imports: vec![],
        text_fixups: vec![],
        data_fixups: (0..fixups)
            .map(|at| Fixup {
                site: 8 * at,
                kind: Kind::Abs64Text,
                bias: 0,
                addend: i64::from(at),
            })
            .collect(),
        entries: vec![Entry {
            identity: IDENTITY,
            signature: Leaf::DIGEST,
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }],
        cfi: vec![],
        sites: vec![],
    }
    .encode()
}

fn admit(loader: &Loader, bytes: &[u8]) {
    let module = Module::parse(bytes).expect("a module");
    loader.admit(module.header.key(), hirsel_mod::digest(bytes));
}

fn report(what: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let at = |q: usize| samples[(samples.len() - 1) * q / 1000];
    println!(
        "{what}: n {} p50 {:?} p99 {:?} p99.9 {:?} max {:?}",
        samples.len(),
        at(500),
        at(990),
        at(999),
        at(1000)
    );
}

fn resident() -> u64 {
    // SAFETY: getrusage fills the record it is given.
    let usage = unsafe {
        let mut usage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    usage.ru_maxrss as u64
}

/// 50,000 distinct small images loaded in batches, then a mixed-lifetime die-off.
fn density(loader: &Loader, align: u32) {
    const IMAGES: u32 = 50_000;
    const LENGTH: usize = 256;
    let before = resident();
    let started = Instant::now();
    let mut codes: Vec<Option<Code<Leaf>>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..16u32)
            .map(|thread| {
                scope.spawn(move || {
                    (thread..IMAGES)
                        .step_by(16)
                        .map(|n| {
                            let bytes = shaped(n, LENGTH, align, 2);
                            admit(loader, &bytes);
                            let code: Code<Leaf> =
                                loader.load(&bytes, &IDENTITY, false).expect("a load");
                            Some(code)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect()
    });
    let took = started.elapsed();
    let mut sum = 0;
    for code in codes.iter().flatten() {
        // SAFETY: the entry is `leaf`.
        sum += unsafe { (code.entry().0)() };
    }
    assert_eq!(sum, u64::from(IMAGES) * u64::from(IMAGES - 1) / 2);
    let grown = (resident() - before) as usize;
    println!(
        "density align {align}: {IMAGES} images of {LENGTH} bytes loaded in {took:?}; \
         installed {} bytes; peak resident grew {grown} KiB ({} bytes an image)",
        loader.installed(),
        grown * 1024 / IMAGES as usize,
    );
    println!("  all live: code mappings {:?}", loader.mappings());
    // Each image dies with the same chance, whatever its neighbours do.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut live = IMAGES as usize;
    for dead in [50, 90, 99, 100] {
        let target = IMAGES as usize * (100 - dead) / 100;
        while live > target {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let at = (state >> 33) as usize % codes.len();
            if codes[at].take().is_some() {
                live -= 1;
            }
        }
        let started = Instant::now();
        loader.reclaim();
        println!(
            "  {dead}% dead: code mappings (all, closed) {:?}; installed {} bytes; reclaim \
             took {:?}; unrevoked {}",
            loader.mappings(),
            loader.installed(),
            started.elapsed(),
            loader.unrevoked(),
        );
    }
}

/// G1c and G3 while every swap's old code dies: each round loads a module the slab does
/// not hold, activates it and drops the handle, so the code it replaces dies when the
/// last worker moves on and the next load reclaims it.
fn releases(loader: &Loader) {
    let modules: Vec<_> = (0..256).map(module).collect();
    for bytes in &modules {
        admit(loader, bytes);
    }
    let slot = loader.slot::<Leaf>();
    for (pace, swaps) in [
        (Duration::ZERO, 50_000usize),
        (Duration::from_micros(50), 20_000),
    ] {
        let stop = AtomicBool::new(false);
        let (mut loads, mut times, mut refused) = (Vec::new(), Vec::new(), 0u64);
        let calls: u64 = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        let mut worker = loader.worker();
                        let mut calls = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            // SAFETY: the entry is `leaf`.
                            worker.dispatch(&slot, |leaf| unsafe { (leaf.0)() });
                            calls += 1;
                        }
                        calls
                    })
                })
                .collect();
            let mut version = slot.version();
            for n in 0..swaps {
                let began = Instant::now();
                let code: Code<Leaf> = loader
                    .load(&modules[n % modules.len()], &IDENTITY, true)
                    .expect("a load");
                loads.push(began.elapsed());
                let started = Instant::now();
                match slot.activate(version, code) {
                    Ok(next) => {
                        version = next;
                        times.push(started.elapsed());
                    }
                    Err(_) => refused += 1,
                }
                while began.elapsed() < pace {
                    std::hint::spin_loop();
                }
            }
            stop.store(true, Ordering::Relaxed);
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum()
        });
        report(
            &format!("G1c activate with last-owner releases, 16 workers, one per {pace:?}"),
            times,
        );
        report("G3 load under reclaim, 16 dispatching workers", loads);
        println!(
            "dispatches meanwhile: {calls}; activates refused: {refused}; installed {} bytes; \
             unrevoked {}",
            loader.installed(),
            loader.unrevoked()
        );
    }
}

fn main() {
    let mode = std::env::args().nth(1);
    let loader = Loader::new(Config {
        abi: hirsel_mod::abi(),
        policy: 0,
        mask: Mask::of(&[
            Class::Pure,
            Class::Metered,
            Class::Task,
            Class::Provider,
            Class::Foreign,
            Class::Exec,
            Class::Launch,
        ]),
        band_bytes: 1 << 30,
        installed_bytes: 512 << 20,
        retired_bytes: Config::RETIRED_BYTES,
        batch: Config::BATCH,
        row: Box::new(|_| Some(stub as *const () as usize)),
        instance: Box::new(|_| None),
        prelinked: &[],
    })
    .expect("a loader");
    match mode.as_deref() {
        Some("density") => {
            let align = std::env::args().nth(2).expect("a text alignment");
            return density(&loader, align.parse().expect("a number"));
        }
        Some("releases") => return releases(&loader),
        _ => {}
    }

    // G3: each load installs, patches and publishes one unseen-by-the-slab module, and
    // each drop leaves its cohort dead, so the slab recycles as it goes.
    const LOADS: u32 = 20_000;
    let modules: Vec<_> = (0..256).map(module).collect();
    for bytes in &modules {
        admit(&loader, bytes);
    }
    let before = resident();
    let mut loads = Vec::new();
    for n in 0..LOADS {
        let bytes = &modules[n as usize % modules.len()];
        let started = Instant::now();
        let code: Code<Leaf> = loader.load(bytes, &IDENTITY, true).expect("a load");
        loads.push(started.elapsed());
        // SAFETY: the entry is `leaf`.
        assert_eq!(unsafe { (code.entry().0)() }, u64::from(n % 256));
    }
    report("G3 load 8 KiB, 200 fixups, flushed", loads);
    println!(
        "G12 {LOADS} loads and unloads: installed {} bytes, peak resident grew {} KiB",
        loader.installed(),
        resident() - before
    );

    // G1c: sixteen workers dispatch while one thread swaps the slot between two codes,
    // back to back and then once every 50 microseconds.
    let codes: Vec<Code<Leaf>> = modules[..2]
        .iter()
        .map(|bytes| loader.load(bytes, &IDENTITY, true).expect("a load"))
        .collect();
    let slot = loader.slot::<Leaf>();
    let mut version = slot
        .activate(0, codes[0].clone())
        .expect("the first activate");
    for (pace, swaps) in [
        (Duration::ZERO, 100_000usize),
        (Duration::from_micros(50), 20_000),
    ] {
        let stop = AtomicBool::new(false);
        let mut times = Vec::new();
        let calls: u64 = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        let mut worker = loader.worker();
                        let mut calls = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            // SAFETY: the entry is `leaf`.
                            let said = worker.dispatch(&slot, |leaf| unsafe { (leaf.0)() });
                            assert!(said.is_some_and(|said| said < 2));
                            calls += 1;
                        }
                        calls
                    })
                })
                .collect();
            for n in 1..=swaps {
                let code = codes[n % 2].clone();
                let started = Instant::now();
                version = slot.activate(version, code).expect("an activate");
                times.push(started.elapsed());
                while started.elapsed() < pace {
                    std::hint::spin_loop();
                }
            }
            stop.store(true, Ordering::Relaxed);
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum()
        });
        report(
            &format!("G1c activate, 16 dispatching workers, one per {pace:?}"),
            times,
        );
        println!("G1c dispatches meanwhile: {calls}");
    }

    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("a module file");
        admit(&loader, &bytes);
        let module = Module::parse(&bytes).expect("a module");
        let (text, data) = (module.text().len(), module.data().len());
        let mut times = Vec::new();
        for _ in 0..200 {
            let started = Instant::now();
            let image = loader.image(&bytes, true);
            times.push(started.elapsed());
            if let Err(refusal) = &image {
                println!("{path}: {refusal:?}");
                break;
            }
        }
        report(&format!("load {path} (text {text}, data {data})"), times);
    }
}
