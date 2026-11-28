//! The pack host of `tools/redteam_ctf`: one process, one pool, two Places.
//!
//! The Canary Place runs trusted Hirsel code on its own execution and worker thread and
//! holds the run's secret in its heap. The Attacker Place is a second execution in the
//! same pool: a trusted warden loads one candidate entry with no capability and runs it
//! under a byte bound, a step bound and `isolate`. Candidates are compiled out of
//! process through the PACK-3 supervisor, so this host links no compiler.
//!
//! The host decides nothing. It answers one command per line on standard input with
//! one line on standard output, and `tools/redteam_ctf/oracle.py` judges those lines,
//! the exit status and the process's memory from outside. `--sabotage` breaks one
//! thing on purpose so the oracle's self-check can prove it reports each breach.
//!
//! usage: ctf_host <hirsel> <scratch dir> <trusted dir> <cranelift|llvm> [--provider]
//!        [--sabotage mode]

use hirsel_code::{Loader, RawCode};
use hirsel_compiler::{Caps, Refusal, Request, Service};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{
    Context,
    process::{Config, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
    tasks::SchedulerProvider,
};
use hirsel_mod::{Backend, Class, Mask, Module};
use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

hirsel_guard::install!();

const ROOM: usize = 4096;
/// The canary answers in microseconds. Past this it is unavailable.
const CANARY_WAIT: Duration = Duration::from_secs(20);

/// `warden/Report`, read through the PACK-15 result place. `warden/layout` proves the
/// field order before any report is trusted.
#[repr(C)]
struct Report {
    stage: u64,
    kind: u64,
    a: u64,
    b: u64,
    causes: u64,
    total: u64,
    len: u64,
    out: [u8; ROOM],
}

impl Report {
    const fn blank() -> Self {
        Self {
            stage: 0,
            kind: 0,
            a: 0,
            b: 0,
            causes: 0,
            total: 0,
            len: 0,
            out: [0; ROOM],
        }
    }
}

/// `canary/Answer`.
#[repr(C)]
#[derive(Default)]
struct Answer {
    words: [u64; 4],
    vault: usize,
}

type Seal = unsafe extern "C" fn(*mut Context, *mut usize, u64, u64, u64, u64) -> u8;
type Respond = unsafe extern "C" fn(*mut Context, *mut Answer, *mut usize, u64, u64, u64) -> u8;
type Retire = unsafe extern "C" fn(*mut Context, *mut u64, *mut usize) -> u8;
type Layout = unsafe extern "C" fn(*mut Context, *mut Report) -> u8;
type Key = *const [u8; 32];
type Engage = unsafe extern "C" fn(*mut Context, *mut Report, u64, Key, Key, u64, u64, u64) -> u8;
type EngageHeld = unsafe extern "C" fn(
    *mut Context,
    *mut Report,
    u64,
    Key,
    Key,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
) -> u8;

unsafe extern "C" fn silent(_: *mut u8, _: *const u8, _: usize) {}

/// Run `body` as the root of `execution` on a contained runtime worker with no grant,
/// no provider and no capability.
fn run<F: FnOnce(*mut Context) -> u8>(
    execution: &Execution,
    root_limit: u64,
    body: F,
) -> Result<u8, EndReason> {
    unsafe extern "C" fn trampoline<F: FnOnce(*mut Context) -> u8>(
        context: *mut Context,
        config: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: `state` is the `Option<F>` below, live for the worker call.
        let slot = unsafe { &mut *(*config).state.cast::<Option<F>>() };
        (slot.take().expect("the root runs once"))(context)
    }
    let mut cell = Some(body);
    let config = Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit,
        args: std::ptr::null(),
        args_len: 0,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: (&raw mut cell).cast(),
        diagnostic: silent,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    };
    // SAFETY: `config` and `cell` outlive the worker, which is joined before return.
    unsafe { run_contained_worker(execution, &config, trampoline::<F>) }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len() / 2)
        .map(|at| u8::from_str_radix(text.get(2 * at..2 * at + 2)?, 16).ok())
        .collect()
}

fn words<const N: usize>(bytes: &[u8]) -> [u64; N] {
    let mut out = [0; N];
    for (at, chunk) in bytes.chunks(8).take(N).enumerate() {
        let mut word = [0; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        out[at] = u64::from_le_bytes(word);
    }
    out
}

fn say(line: std::fmt::Arguments<'_>) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

fn end(reason: EndReason) -> String {
    match reason {
        EndReason::RunStop(stop) => format!("run-stop name={}", hex(stop.as_bytes())),
        EndReason::HostStop => "host-stop".into(),
    }
}

/// The identity the capped worker gives `local/app@1/<path>`.
fn identity(path: &str) -> ContentHash {
    let name = format!("local/app@1/{path}");
    let zero = 0u64.to_le_bytes();
    ContentHash::of_parts("hirsel.core-symbol.v1", [name.as_bytes(), &zero, &zero])
}

struct Host {
    service: Service,
    backend: Backend,
    loader: &'static Loader,
    pool: u64,
}

impl Host {
    fn compile(&self, file: &str, source: &[u8], exports: &[&str]) -> Result<Vec<u8>, Refusal> {
        let exports: Vec<String> = exports
            .iter()
            .map(|export| format!("local/app@1/{export}"))
            .collect();
        let exports: Vec<&str> = exports.iter().map(String::as_str).collect();
        let request = Request {
            files: &[(file, source)],
            target: hirsel_code::TARGET,
            backend: self.backend,
            policy: 0,
        };
        Ok(self.service.compile_exports(&request, &exports)?.bytes)
    }

    /// A trusted module: compiled by the capped worker like any other, then admitted
    /// by this host's policy.
    fn trusted(&self, dir: &str, module: &str, exports: &[&str]) -> (Vec<u8>, ContentHash) {
        let file = format!("{module}.hirsel");
        let source = std::fs::read(format!("{dir}/{file}")).expect("a trusted source");
        let exports: Vec<String> = exports.iter().map(|e| format!("{module}/{e}")).collect();
        let exports: Vec<&str> = exports.iter().map(String::as_str).collect();
        let bytes = self
            .compile(&file, &source, &exports)
            .unwrap_or_else(|refusal| panic!("trusted module {module}: {refusal:?}"));
        let (key, _) = self.loader.admit_module(&bytes).expect("a trusted module");
        (bytes, key)
    }

    fn entry(&self, bytes: &[u8], key: &ContentHash, path: &str) -> RawCode {
        let id = identity(path);
        let module = Module::parse(bytes).expect("a trusted module");
        let row = module
            .entry(&id)
            .unwrap_or_else(|| panic!("no entry {path}"));
        self.loader
            .load_by_key(key, &id, &row.signature, row.waits, true)
            .unwrap_or_else(|refusal| panic!("load {path}: {refusal:?}"))
    }
}

enum Ask {
    Seal([u64; 4]),
    Probe([u64; 3]),
    Retire,
}

enum Told {
    Sealed,
    Answer([u64; 4]),
    Retired,
    Failed(&'static str, u8),
}

/// The Canary Place's root: it owns the vault handle and serves asks until retired.
fn canary(
    context: *mut Context,
    entries: (Seal, Respond, Retire),
    asks: &Receiver<Ask>,
    told: &Sender<Told>,
) -> u8 {
    let (seal, respond, retire) = entries;
    let mut vault = 0usize;
    for ask in asks {
        let reply = match ask {
            Ask::Seal(w) => {
                // SAFETY: `seal` is the trusted canary's entry at its checked signature.
                match unsafe { seal(context, &mut vault, w[0], w[1], w[2], w[3]) } {
                    0 => Told::Sealed,
                    status => Told::Failed("seal", status),
                }
            }
            Ask::Probe(n) => {
                let mut answer = Answer::default();
                // SAFETY: as above; `respond` consumes the vault and returns a new one.
                match unsafe { respond(context, &mut answer, &mut vault, n[0], n[1], n[2]) } {
                    0 => {
                        vault = answer.vault;
                        Told::Answer(answer.words)
                    }
                    status => Told::Failed("respond", status),
                }
            }
            Ask::Retire => {
                let mut length = 0;
                // SAFETY: as above; `retire` consumes the vault.
                let status = unsafe { retire(context, &mut length, &mut vault) };
                let _ = told.send(if status == 0 && length == 32 {
                    Told::Retired
                } else {
                    Told::Failed("retire", status)
                });
                return status;
            }
        };
        if told.send(reply).is_err() {
            return 0;
        }
    }
    0
}

struct Canary {
    asks: Sender<Ask>,
    told: Receiver<Told>,
    thread: Option<std::thread::JoinHandle<Result<u8, EndReason>>>,
}

impl Canary {
    /// One ask and its reply, or why the canary did not answer.
    fn ask(&mut self, ask: Ask) -> Result<Told, String> {
        if self.asks.send(ask).is_ok() {
            match self.told.recv_timeout(CANARY_WAIT) {
                Ok(told) => return Ok(told),
                Err(RecvTimeoutError::Timeout) => return Err("silent".into()),
                Err(RecvTimeoutError::Disconnected) => {}
            }
        }
        Err(
            match self.thread.take().map(std::thread::JoinHandle::join) {
                Some(Ok(Ok(status))) => format!("returned status={status}"),
                Some(Ok(Err(reason))) => end(reason),
                Some(Err(_)) => "panicked".into(),
                None => "gone".into(),
            },
        )
    }

    fn line(&mut self, ask: Ask) -> String {
        match self.ask(ask) {
            Ok(Told::Sealed) => "sealed".into(),
            Ok(Told::Answer(w)) => {
                let bytes: Vec<u8> = w.iter().flat_map(|word| word.to_be_bytes()).collect();
                format!("answer {}", hex(&bytes))
            }
            Ok(Told::Retired) => "retired".into(),
            Ok(Told::Failed(what, status)) => format!("canary-lost {what} status={status}"),
            Err(why) => format!("canary-lost {why}"),
        }
    }
}

struct Attack<'a> {
    arena: &'a str,
    steps: u64,
    bytes: u64,
    root: u64,
    wall_ms: u64,
    path: &'a str,
}

struct Wardens {
    engage: Engage,
    engage_held: EngageHeld,
}

fn compile_line(refusal: &Refusal) -> String {
    let (kind, text) = match refusal {
        Refusal::Rejected(text) => ("Rejected", text.clone()),
        Refusal::Unsupported(text) => ("Unsupported", text.clone()),
        Refusal::Limit(limit) => ("Limit", format!("{limit:?}")),
        Refusal::Busy => ("Busy", String::new()),
        Refusal::ToolRefusal(text) => ("ToolRefusal", text.clone()),
        Refusal::Unavailable(text) => ("Unavailable", text.clone()),
    };
    format!("compile-refused kind={kind} text={}", hex(text.as_bytes()))
}

/// Compile, admit and run one candidate in a fresh Attacker Place.
fn attack(host: &Host, wardens: &Wardens, attack: &Attack<'_>, decoy: [u64; 4]) -> String {
    let Ok(source) = std::fs::read(attack.path) else {
        return "bad-command unreadable".into();
    };
    let bytes = match host.compile("main.hirsel", &source, &["main/attack"]) {
        Ok(bytes) => bytes,
        Err(refusal) => return compile_line(&refusal),
    };
    // Admission is this host's policy: it admits what its own capped compile produced.
    let key = match host.loader.admit_module(&bytes) {
        Ok((key, _)) => key,
        Err(refusal) => return format!("admit-refused {}", hex(format!("{refusal:?}").as_bytes())),
    };
    let id = identity("main/attack");
    let Some(execution) = Execution::in_pool(host.pool) else {
        return "bad-command pool".into();
    };
    let mut report = Report::blank();
    let (done, wait) = channel::<()>();
    let ended = std::thread::scope(|scope| {
        // The wall bound: an orderly host stop of the Attacker Place alone.
        let (place, wall) = (&execution, Duration::from_millis(attack.wall_ms));
        scope.spawn(move || {
            if wait.recv_timeout(wall) == Err(RecvTimeoutError::Timeout) {
                place.request_host_stop();
            }
        });
        let ended = run(&execution, attack.root, |context| {
            let (pool, steps, limit) = (host.pool, attack.steps, attack.bytes);
            // SAFETY: the warden's entries at their checked signatures; `report`, the key
            // and the identity outlive the call.
            unsafe {
                if attack.arena == "held" {
                    let [a, b, c, d] = decoy;
                    (wardens.engage_held)(
                        context,
                        &mut report,
                        pool,
                        &key.0,
                        &id.0,
                        steps,
                        limit,
                        a,
                        b,
                        c,
                        d,
                    )
                } else {
                    (wardens.engage)(context, &mut report, pool, &key.0, &id.0, steps, limit, 0)
                }
            }
        });
        let _ = done.send(());
        ended
    });
    let line = match ended {
        Ok(0) => {
            let shown = (report.len as usize).min(ROOM);
            format!(
                "report stage={} kind={} a={} b={} causes={} total={} out={}",
                report.stage,
                report.kind,
                report.a,
                report.b,
                report.causes,
                report.total,
                hex(&report.out[..shown])
            )
        }
        Ok(status) => format!("escaped status={status}"),
        Err(reason) => {
            let fault = hirsel_pack::report(&execution);
            format!(
                "{} fault={} quarantined={}",
                end(reason),
                u8::from(fault.is_some()),
                u8::from(fault.is_some_and(|fault| fault.quarantined.is_some()))
            )
        }
    };
    drop(execution);
    let _ = host.loader.reclaim();
    line
}

/// Offer bytes this host's compile did not produce. No admission is minted for them.
fn raw(host: &Host, path: &str) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return "bad-command unreadable".into();
    };
    let id = identity("main/attack");
    let line = match host
        .loader
        .load_entry_raw(&bytes, &id, &ContentHash([0; 32]), false, true)
    {
        Ok(_) => "raw-loaded".into(),
        Err(refusal) => format!("raw-refused {}", hex(format!("{refusal:?}").as_bytes())),
    };
    let _ = host.loader.reclaim();
    line
}

/// Compile a candidate, admit it and write its module out, so the oracle can offer
/// altered copies of bytes whose original this host admits.
fn emit(host: &Host, source: &str, output: &str) -> String {
    let Ok(source) = std::fs::read(source) else {
        return "bad-command unreadable".into();
    };
    match host.compile("main.hirsel", &source, &["main/attack"]) {
        Ok(bytes) => match host.loader.admit_module(&bytes) {
            Ok(_) if std::fs::write(output, &bytes).is_ok() => {
                format!("emitted bytes={}", bytes.len())
            }
            Ok(_) => "bad-command unwritable".into(),
            Err(refusal) => format!("admit-refused {}", hex(format!("{refusal:?}").as_bytes())),
        },
        Err(refusal) => compile_line(&refusal),
    }
}

/// Break one thing on purpose. `kept` is the secret, which only a sabotaged host keeps.
fn sabotage(mode: &str, kept: &[u8], canary: &mut Canary) -> Option<String> {
    let leak = |bytes: &[u8]| {
        Some(format!(
            "report stage=2 kind=1 a=0 b=0 causes=0 total={} out={}",
            bytes.len(),
            hex(bytes)
        ))
    };
    match mode {
        "leak" => leak(kept),
        "leak-window" => leak(&kept[11..20]),
        "leak-hex" => leak(hex(kept).as_bytes()),
        "leak-cause" => Some(format!(
            "report stage=2 kind=4 a=5 b=0 causes=1 total=32 out={}",
            hex(kept)
        )),
        "leak-stderr" => {
            std::io::stderr().write_all(kept).unwrap();
            None
        }
        "crash-abort" => std::process::abort(),
        "crash-segv" => {
            // SAFETY: none; the write to the null page is the sabotage.
            unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
            None
        }
        "hang" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        "rss" => {
            let mut held = Vec::new();
            loop {
                held.push(vec![0x5au8; 32 << 20]);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "canary-swap" => {
            let _ = canary.ask(Ask::Seal([1, 2, 3, 4]));
            None
        }
        "canary-kill" => {
            let _ = canary.ask(Ask::Retire);
            None
        }
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [worker, scratch, trusted, engine, rest @ ..] = &args[..] else {
        eprintln!(
            "usage: ctf_host <hirsel> <scratch> <trusted> <cranelift|llvm> [--provider] [--sabotage mode]"
        );
        std::process::exit(2);
    };
    // `--provider` widens the pool's import mask by the virtual providers (CAP-2), whose
    // ports the program itself serves. They reach no host authority.
    let (provider, rest) = match rest {
        [flag, rest @ ..] if flag == "--provider" => (true, rest),
        rest => (false, rest),
    };
    let mode = match rest {
        [flag, mode] if flag == "--sabotage" => mode.as_str(),
        _ => "",
    };
    let mask = if provider {
        Mask::of(&[Class::Pure, Class::Metered, Class::Task, Class::Provider])
    } else {
        Mask::PURE
    };
    let backend = if engine == "llvm" {
        Backend::Llvm
    } else {
        Backend::Cranelift
    };
    let loader = hirsel_pack::start(hirsel_pack::config(0, mask, 64 << 20, 32 << 20))
        .expect("the pack pool");
    let host = Host {
        service: Service::new(worker.into(), scratch.into(), Caps::HOST, 1, 0),
        backend,
        loader,
        pool: hirsel_pack::loader_handle(loader).expect("the pool's handle"),
    };
    let (canary_bytes, canary_key) =
        host.trusted(trusted, "canary", &["seal", "respond", "retire"]);
    let (warden_bytes, warden_key) =
        host.trusted(trusted, "warden", &["engage", "engage_held", "layout"]);
    let code = |bytes: &[u8], key: &ContentHash, path: &str| host.entry(bytes, key, path);
    let seal = code(&canary_bytes, &canary_key, "canary/seal");
    let respond = code(&canary_bytes, &canary_key, "canary/respond");
    let retire = code(&canary_bytes, &canary_key, "canary/retire");
    let engage = code(&warden_bytes, &warden_key, "warden/engage");
    let engage_held = code(&warden_bytes, &warden_key, "warden/engage_held");
    let layout = code(&warden_bytes, &warden_key, "warden/layout");
    // SAFETY: each entry was loaded at the signature its own module's row names, and
    // these are the PACK-15 shapes of the trusted sources' declarations.
    let (entries, wardens, layout): ((Seal, Respond, Retire), Wardens, Layout) = unsafe {
        (
            (
                std::mem::transmute::<usize, Seal>(seal.entry()),
                std::mem::transmute::<usize, Respond>(respond.entry()),
                std::mem::transmute::<usize, Retire>(retire.entry()),
            ),
            Wardens {
                engage: std::mem::transmute::<usize, Engage>(engage.entry()),
                engage_held: std::mem::transmute::<usize, EngageHeld>(engage_held.entry()),
            },
            std::mem::transmute::<usize, Layout>(layout.entry()),
        )
    };

    let mut proof = Report::blank();
    let checked = Execution::in_pool(host.pool).expect("the pool");
    // SAFETY: `layout` is the warden's entry; `proof` outlives the call.
    let status = run(&checked, 1 << 20, |context| unsafe {
        layout(context, &mut proof)
    });
    let fields = [
        proof.stage,
        proof.kind,
        proof.a,
        proof.b,
        proof.causes,
        proof.total,
    ];
    let expect: Vec<u64> = (1..=6).map(|n| n * 0x1111_1111_1111_1111).collect();
    let bytes = (proof.out[0], proof.out[1], proof.out[ROOM - 1]);
    if status != Ok(0) || fields[..] != expect[..] || proof.len != 3 || bytes != (0xc7, 0xf0, 0xa5)
    {
        eprintln!("ctf_host: the warden's report layout is not the one this host reads");
        std::process::exit(3);
    }
    drop(checked);

    let (asks, canary_asks) = channel();
    let (canary_told, told) = channel();
    let pool = host.pool;
    let thread = std::thread::spawn(move || {
        let execution = Execution::in_pool(pool).expect("the pool");
        run(&execution, 1 << 20, |context| {
            canary(context, entries, &canary_asks, &canary_told)
        })
    });
    let mut canary = Canary {
        asks,
        told,
        thread: Some(thread),
    };

    say(format_args!("ready pid={}", std::process::id()));
    let mut kept = Vec::new();
    let mut decoy = [0u64; 4];
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let parts: Vec<&str> = line.split(' ').collect();
        let reply = match parts[..] {
            ["seal", secret, lure] => match (unhex(secret), unhex(lure)) {
                (Some(secret), Some(lure)) if secret.len() == 32 && lure.len() == 32 => {
                    if !mode.is_empty() {
                        kept.clone_from(&secret);
                    }
                    decoy = words(&lure);
                    canary.line(Ask::Seal(words(&secret)))
                }
                _ => "bad-command seal".into(),
            },
            ["probe", nonce] => match unhex(nonce) {
                Some(nonce) if nonce.len() == 23 => canary.line(Ask::Probe(words(&nonce))),
                _ => "bad-command probe".into(),
            },
            ["attack", arena, steps, bytes, root, wall_ms, path] => {
                let numbers = [steps, bytes, root, wall_ms].map(str::parse::<u64>);
                match numbers {
                    [Ok(steps), Ok(bytes), Ok(root), Ok(wall_ms)] => {
                        let attack_request = Attack {
                            arena,
                            steps,
                            bytes,
                            root,
                            wall_ms,
                            path,
                        };
                        sabotage(mode, &kept, &mut canary)
                            .unwrap_or_else(|| attack(&host, &wardens, &attack_request, decoy))
                    }
                    _ => "bad-command attack".into(),
                }
            }
            ["emit", source, output] => emit(&host, source, output),
            ["raw", path] => raw(&host, path),
            ["quit"] => break,
            _ => "bad-command unknown".into(),
        };
        say(format_args!("{reply}"));
    }
    let retired = canary.line(Ask::Retire);
    let joined = canary.thread.take().map(std::thread::JoinHandle::join);
    let clean = retired == "retired" && matches!(joined, Some(Ok(Ok(0))));
    drop((seal, respond, retire, engage, engage_held, layout));
    let _ = host.loader.reclaim();
    say(format_args!(
        "bye canary={retired} live={}",
        host.loader.live_images()
    ));
    std::process::exit(i32::from(!clean));
}
