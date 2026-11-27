// Contained foreign laws (docs/spec/foreign.md, "Contained laws"). The refusal
// modules are written here byte by byte from the WebAssembly binary format, so
// each one holds exactly the construct its law names.

struct ContainedExecution {
    target: &'static str,
    runtime: PathBuf,
    cc: PathBuf,
    runner: crate::Runner,
}

impl ContainedExecution {
    fn new(law: &PackageLaw) -> Self {
        if let Some(runtime) = std::env::var_os("HIRSEL_CONTAINED_RUNTIME") {
            let input = |name| PathBuf::from(std::env::var_os(name).unwrap());
            let target = hirsel_core_layout::Target::Aarch64Linux.triple();
            let runner = crate::Runner::qemu(&input("HIRSEL_QEMU_AARCH64"), &input("HIRSEL_AARCH64_SYSROOT"), target).unwrap();
            Self { target, runtime: runtime.into(), cc: input("HIRSEL_AARCH64_CC"), runner }
        } else {
            let options = native_options(Backend::Cranelift, law.dir.path());
            let Link::Driver(cc) = options.link else { unreachable!() };
            Self {
                target: crate::host::HOST_TARGET.triple(),
                runtime: options.runtime.canonicalize().unwrap(),
                cc: cc.canonicalize().unwrap(),
                runner: crate::Runner::Native,
            }
        }
    }

    fn engines() -> &'static [&'static str] {
        match std::env::var("HIRSEL_CONTAINED_TEST_ENGINE").as_deref() {
            Ok("cranelift") => &["cranelift"],
            Ok("llvm") => &["llvm"],
            Err(std::env::VarError::NotPresent) => &["cranelift", "llvm"],
            other => panic!("unknown contained test engine: {other:?}"),
        }
    }

    fn link_args<'a>(&'a self, runtime: &'a Path) -> [&'a str; 4] {
        ["--runtime", runtime.to_str().unwrap(), "--cc", self.cc.to_str().unwrap()]
    }
}

/// A core module: the preamble, then each section as its id and framed body.
fn wasm_module(sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    for (id, body) in sections {
        module.push(*id);
        wasm_frame(&mut module, body);
    }
    module
}

fn wasm_frame(out: &mut Vec<u8>, bytes: &[u8]) {
    let mut n = bytes.len();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        out.push(if n == 0 { byte } else { byte | 0x80 });
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(bytes);
}

fn wasm_name(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    wasm_frame(&mut out, text.as_bytes());
    out
}

/// One function type per entry, each `(params, results)` as value type bytes.
fn wasm_types(types: &[(&[u8], &[u8])]) -> (u8, Vec<u8>) {
    let mut body = vec![types.len() as u8];
    for (params, results) in types {
        body.push(0x60);
        wasm_frame(&mut body, params);
        wasm_frame(&mut body, results);
    }
    (1, body)
}

/// One memory of one page, exported as `memory`, then function exports.
fn wasm_memory() -> (u8, Vec<u8>) {
    (5, vec![1, 0, 1])
}

fn wasm_exports(functions: &[&str]) -> (u8, Vec<u8>) {
    let mut body = vec![functions.len() as u8 + 1];
    body.extend(wasm_name("memory"));
    body.extend([2, 0]);
    for (index, name) in functions.iter().enumerate() {
        body.extend(wasm_name(name));
        body.extend([0, index as u8]);
    }
    (7, body)
}

/// A code section of bodies with no locals.
fn wasm_code(bodies: &[&[u8]]) -> (u8, Vec<u8>) {
    let mut body = vec![bodies.len() as u8];
    for code in bodies {
        wasm_frame(&mut body, &[&[0u8][..], code].concat());
    }
    (10, body)
}

/// One `[] -> []` function whose body is `code`, beside `extra` sections.
fn wasm_function(extra: &[(u8, Vec<u8>)], code: &[u8]) -> Vec<u8> {
    let mut sections = vec![wasm_types(&[(&[], &[])]), (3, vec![1, 0])];
    sections.extend_from_slice(extra);
    sections.push(wasm_code(&[code]));
    wasm_module(&sections)
}

fn wasm_import(module: &str, name: &str) -> Vec<u8> {
    let mut body = vec![1];
    body.extend(wasm_name(module));
    body.extend(wasm_name(name));
    body.extend([0, 0]);
    wasm_module(&[wasm_types(&[(&[], &[])]), (2, body), wasm_memory(), wasm_exports(&[])])
}

#[test]
fn contained_modules_are_admitted_before_anything_compiles() {
    use sha2::{Digest, Sha256};
    use std::fs;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const MODULE: &str = "FFI-11: module box.wasm of hirsel.ai/wasm_laws@1/native";
    const I32: u8 = 0x7f;
    const I64: u8 = 0x7e;

    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/wasm_laws");
    for file in ["laws.hirsel", "native.hirsel"] {
        law.write(&format!("wasm_laws/{file}"), fs::read(fixture.join(file)).unwrap());
    }
    law.write("wasm_laws/main.hirsel", r#"
pub fn main(f: Foreign[laws.Box]) -> Result[(), Exit] {
    let mut b: Bytes = list.with_capacity(4)
    let _ = native.fill(f, mut b)
    Ok(())
}
"#);
    let original = fs::read_to_string(fixture.join("hirsel.toml")).unwrap();
    let (head, _) = original.split_once("containment").unwrap();
    let manifest = |table: &str| law.write("wasm_laws/hirsel.toml", format!("{head}{table}\n"));
    let repin = |bytes: &[u8], pin: Option<&str>| {
        law.write("wasm_laws/box.wasm", bytes);
        let pin = pin.map_or_else(|| hirsel_core_ids::hex::encode(&Sha256::digest(bytes)), str::to_owned);
        manifest(&format!("containment = \"wasm\"\nmodule = {{ file = \"box.wasm\", sha256 = \"{pin}\" }}"));
    };
    let execution = ContainedExecution::new(&law);
    let build = |engine: &str| {
        let request = [
            "build", "wasm_laws", "--backend", engine, "--target", execution.target,
            "--libraries", law.libraries.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", "image",
            "--grant", "Foreign=hirsel.ai/wasm_laws@1/laws/Box",
        ].into_iter().chain(execution.link_args(&execution.runtime)).map(str::to_owned).collect::<Vec<_>>();
        process::tape_request(&request, law.dir.path())
    };
    let refused = |engine: &str, expected: &dyn Fn(&str) -> bool, what: &str| {
        let (status, output, error) = build(engine);
        assert_eq!(status, Some(2), "{what}\n{output}\n{error}");
        assert!(error.lines().any(expected), "{what}: {error}");
        assert!(!law.path("image").exists(), "{what}: an image was written");
    };
    let line = |want: String| move |line: &str| line == want;
    let invalid = |line: &str| line.starts_with(&format!("{MODULE} is not valid with the admitted features: "));

    // A one-page memory exported as `memory`, `hwl_fill` and `hwl_loan_area`.
    let fill_types = wasm_types(&[(&[I32, I32], &[I64]), (&[I32], &[I32])]);
    let fill = [0x42, 0x00, 0x0b]; // i64.const 0
    let area = [0x41, 0x08, 0x0b]; // i32.const 8
    let no_abi = wasm_module(&[fill_types, (3, vec![2, 0, 1]), wasm_memory(),
        wasm_exports(&["hwl_fill", "hwl_loan_area"]), wasm_code(&[&fill, &area])]);
    let wrong_type = wasm_module(&[wasm_types(&[(&[I32], &[I64])]), (3, vec![1, 0]), wasm_memory(),
        wasm_exports(&["hwl_fill"]), wasm_code(&[&fill])]);
    // i32.const 0; i32.const 1; i32.atomic.rmw.add; drop, on a shared memory.
    let atomic = wasm_function(&[(5, vec![1, 3, 1, 1])], &[0x41, 0, 0x41, 1, 0xfe, 0x1e, 2, 0, 0x1a, 0x0b]);
    // Three v128.const zeros, f32x4.relaxed_madd and drop.
    let mut relaxed = Vec::new();
    for _ in 0..3 {
        relaxed.extend([0xfd, 0x0c]);
        relaxed.extend([0; 16]);
    }
    relaxed.extend([0xfd, 0x85, 0x02, 0x1a, 0x0b]);
    let relaxed = wasm_function(&[], &relaxed);
    let memory64 = wasm_module(&[(5, vec![1, 4, 1])]);
    let externref = wasm_module(&[(4, vec![1, 0x6f, 0, 1])]);
    let component = b"\0asm\x0d\0\x01\0".to_vec();

    for &engine in ContainedExecution::engines() {
        // FW1: the pin is checked before a byte is parsed.
        repin(b"abc", Some(EMPTY));
        refused(engine, &line(format!("{MODULE} does not match its pin: expected sha256 {EMPTY}, found sha256 {ABC}")), "FW1 pin");
        repin(b"abc", Some(ABC));
        refused(engine, &line(format!("{MODULE} is not a core WebAssembly module")), "FW1 bytes");
        // FW2: a component, and each disabled proposal.
        repin(&component, None);
        refused(engine, &line(format!("{MODULE} is not a core WebAssembly module")), "FW2 component");
        for (module, what) in [(&atomic, "atomics"), (&relaxed, "relaxed SIMD"), (&memory64, "memory64"), (&externref, "externref")] {
            repin(module, None);
            refused(engine, &invalid, what);
        }
        // FW3: an import that is not a null import.
        for (module, name) in [("wasi_snapshot_preview1", "path_open"), ("env", "getenv")] {
            repin(&wasm_import(module, name), None);
            refused(engine, &line(format!("{MODULE} imports {module}.{name}, which is not a null import")), name);
        }
        // FW4: an export of the wrong type, and a module with no ABI text.
        repin(&wrong_type, None);
        refused(engine, &line(format!("{MODULE} exports hwl_fill as [i32] -> [i64], but its header gives [i32 i32] -> [i64]")), "FW4 type");
        repin(&no_abi, None);
        refused(engine, &line(format!("{MODULE} has no ABI text")), "FW4 ABI");
    }

    // FW20: after W1 the fixture module builds; `native` is not a containment.
    law.write("wasm_laws/box.wasm", fs::read(fixture.join("box.wasm")).unwrap());
    law.write("wasm_laws/hirsel.toml", original.as_str());
    for &engine in ContainedExecution::engines() {
        let (status, output, error) = build(engine);
        assert_eq!(status, Some(0), "FW20 builds\n{output}\n{error}");
        assert!(law.path("image").exists(), "FW20: no image");
        fs::remove_file(law.path("image")).unwrap();
    }
    law.write("wasm_laws/box.wasm", b"abc");
    manifest(&format!("containment = \"native\"\nmodule = {{ file = \"box.wasm\", sha256 = \"{ABC}\" }}"));
    refused(ContainedExecution::engines()[0], &|line: &str| line.ends_with("hirsel.toml: foreign.native: containment: expected \"wasm\""), "FW20");
}

/// FW3, FW5 to FW16, FW21 to FW26, FW28 and FW31 on `box.wasm`, each observation
/// printed by `run.hirsel` and compared with the spec's value. The step
/// counts are hand-derived from `box.wasm`'s operators and the pinned cost
/// table: `spin(f, 10⁴)` is 7 + 9 × 10⁴ + 5 = 90,012 fuel, 5,626 steps, plus
/// the call's; `trap(f, 7)` stores 7 fuel at its trap, so 1 step pays it; and
/// `loop_div(f, 10⁵)` is `L` = 7 + 8 × 10⁵ + 6 = 800,013 fuel.
#[test]
fn contained_calls_run_in_the_tasks_own_instance() {
    use std::fs;
    use std::process::Command;

    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/wasm_laws");
    for file in ["hirsel.toml", "laws.hirsel", "native.hirsel", "box.wasm"] {
        law.write(&format!("wasm_laws/{file}"), fs::read(fixture.join(file)).unwrap());
    }
    law.write("wasm_laws/main.hirsel", fs::read(fixture.join("run.hirsel")).unwrap());
    let execution = ContainedExecution::new(&law);
    let trap = |x: &str| format!("contained trap: {x}");
    let expected: [(&str, Vec<String>); 10] = [
        ("loans", [
            "FW3 clock 52", "FW5 fill 4", "FW5 len 4", "FW5 marked 1", "FW5 sum 22", "FW5 empty 0",
            &trap("unreachable"), "FW5 kept",
            // SEM-2's NaN, and f64.div's canonical NaN 0x7ff8000000000000.
            "FW11 nan32 2143289344", "FW11 div 9221120237041090560",
        ].map(str::to_owned).to_vec()),
        ("area", vec!["contained loan area out of bounds".into(), "FW6 count 2".into()]),
        ("traps", [
            "unreachable", "memory out of bounds", "table out of bounds", "indirect call to null",
            "bad signature", "integer overflow", "integer division by zero",
            "bad conversion to integer", "stack overflow", "exit 7",
        ].into_iter().flat_map(|x| [trap(x), "FW12 count 1".into()])
            .chain([trap("stack overflow"), "FW16 after".into()]).collect()),
        ("handles", vec![
            trap("unreachable"), "foreign instance discarded".into(), "foreign handle released".into(),
            "foreign instance discarded".into(), "FW13 count 1".into(),
            "foreign instance discarded".into(), "FW26 live 1".into(),
        ]),
        // FW31: dirty reads back 0xAB; clean sees count 1, byte 0, 2 pages, one page at 2, then −1.
        ("tasks", ["FW14 one 123", "FW14 two 123", "FW25 one 0", "FW25 two 0", "FW31 dirty 171", "FW31 clean 10220"].map(str::to_owned).to_vec()),
        ("boxes", [
            "FW15 two 2", "FW15 dropped 1", "FW15 freed 0", "x", "FW15 released 0",
        ].map(str::to_owned).to_vec()),
        ("steps", [
            "FW7 spin 1e9 out 1000", "FW7 spin 10 ok 10",
            "FW21 straight out 2", "FW21 straight_trap out 2", "FW21 straight 300 ok 2000",
            "FW23 fill 100 out 100", "FW23 fill 1e4 ok 171",
            "FW24 spin out 1000", "FW24 count 1", "foreign instance discarded",
        ].map(str::to_owned).to_vec()),
        // B's line first: A's call ends its slices under its bound.
        ("slices", ["FW9 B", "FW9 A 100000000"].map(str::to_owned).to_vec()),
        // memory.grow's −1, plus one; the old size; the initial 2 pages refused;
        // a fresh instance; and the child's grow, whose old size was 2 pages.
        ("bytes", [
            "FW10 big+1 0", "FW10 one 2", "capacity 131072", "FW10 count 1", "FW10 child 2",
        ].map(str::to_owned).to_vec()),
        // FW7, FW8 and FW22's bisections; the same on 3 runs below.
        ("bisect", ["FW7 least 5627", "FW8 least 2"].map(str::to_owned).to_vec()),
    ];
    let fw22 = |line: &str| {
        let n: u64 = line.strip_prefix("FW22 least ").and_then(|n| n.parse().ok()).unwrap_or(0);
        let least = 2 * 800_013u64.div_ceil(16);
        (least..=least + 1_030).contains(&n)
    };
    for &engine in ContainedExecution::engines() {
        let image = law.path(&format!("image-{engine}"));
        let request = [
            "build", "wasm_laws", "--backend", engine, "--target", execution.target,
            "--libraries", law.libraries.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", image.to_str().unwrap(),
            "--grant", "Foreign=hirsel.ai/wasm_laws@1/laws/Box", "--grant", "Tasks", "--grant", "Stdout",
            "--limit", "268435456",
        ].into_iter().chain(execution.link_args(&execution.runtime)).map(str::to_owned).collect::<Vec<_>>();
        let (status, output, error) = process::tape_request(&request, law.dir.path());
        assert_eq!(status, Some(0), "{engine}: build\n{output}\n{error}");
        let run = |mode: &str, threads: &str| {
            let command = execution.runner.command(&image);
            Command::new("timeout").arg("120s").arg(command.get_program()).args(command.get_args()).arg(mode)
                .env("HIRSEL_UNTIED", threads).current_dir(law.dir.path()).output().unwrap()
        };
        for (mode, lines) in &expected {
            let runs = if *mode == "bisect" { 3 } else { 1 };
            for _ in 0..runs {
                let out = run(mode, "1");
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(out.status.success(), "{engine} {mode}: {:?}\n{stdout}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
                let mut seen: Vec<&str> = stdout.lines().collect();
                if *mode == "bisect" {
                    assert!(seen.pop().is_some_and(fw22), "{engine} FW22: {stdout}");
                }
                assert_eq!(seen, *lines, "{engine} {mode}");
            }
        }
        // FW28: a guest fault on one thread beside a Hirsel overflow on another.
        let out = run("signals", "2");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{engine} signals: {:?}\n{stderr}", out.status);
        assert!(stderr.starts_with("Stack at "), "{engine} signals: {stderr}");
        assert!(!stdout.is_empty() && stdout.lines().all(|line| line == trap("memory out of bounds")), "{engine} signals: {stdout}");
    }
}

/// FW30: a host memory refusal in contained code stops the run with
/// `HostMemory` (status 102, FFI-13 Bytes / FAIL-15) instead of falling into
/// `trap_text`'s abort arm: the reservation of a fresh slot under `RLIMIT_AS`
/// = 2 GiB (below the 4 GiB + 64 MiB reservation), and a granted grow on a
/// reused slot under `RLIMIT_DATA` = 96 MiB, whose 128 MiB `mprotect` the host
/// refuses.
#[test]
fn contained_host_memory_refusal_stops_with_host_memory() {
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/wasm_laws");
    for file in ["hirsel.toml", "laws.hirsel", "native.hirsel", "box.wasm"] {
        law.write(&format!("wasm_laws/{file}"), fs::read(fixture.join(file)).unwrap());
    }
    law.write(
        "wasm_laws/main.hirsel",
        r#"fn first(take f: Foreign[laws.Box]) -> u64 {
    native.count(f)
}

fn grows(take parts: (Foreign[laws.Box], Stdout)) -> i32 waits {
    let (f, out) = take parts
    let _ = native.count(f)
    result.unwrap(stdout.write(out, "FW30 grow\n"))
    native.grow(f, 2048)
}

pub fn main(f: Foreign[laws.Box], tasks: Tasks, argv: Args, out: Stdout) -> Result[(), Exit] waits {
    result.unwrap(stdout.write(out, "FW30 before\n"))
    let Some(mode) = args.get(argv, 0) else {
        return Error(Exit(status: 3))
    }
    if mode == "reserve" {
        let _ = native.count(f)
        return Ok(())
    }
    let one = task.spawn(tasks, bytes 1048576, f, first)
    let _ = task.join(take one)
    let two = task.spawn(tasks, bytes 201326592, (f, out), grows)
    let _ = task.join(take two)
    Ok(())
}
"#,
    );
    let options = native_options(Backend::Cranelift, law.dir.path());
    let runtime = options.runtime.canonicalize().unwrap();
    for engine in ["cranelift", "llvm"] {
        let image = law.path(&format!("image-{engine}"));
        let request = [
            "build", "wasm_laws", "--backend", engine,
            "--libraries", law.libraries.to_str().unwrap(), "--runtime", runtime.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", image.to_str().unwrap(),
            "--grant", "Foreign=hirsel.ai/wasm_laws@1/laws/Box", "--grant", "Stdout", "--grant", "Tasks",
            "--limit", "268435456",
        ].map(str::to_owned);
        let (status, output, error) = process::tape_request(&request, law.dir.path());
        assert_eq!(status, Some(0), "{engine}: build\n{output}\n{error}");
        for (mode, resource, bytes, lines) in [
            ("reserve", libc::RLIMIT_AS, 2u64 << 30, "FW30 before\n"),
            ("grow", libc::RLIMIT_DATA, 96 << 20, "FW30 before\nFW30 grow\n"),
        ] {
            let mut command = Command::new(&image);
            command.arg(mode).env("HIRSEL_UNTIED", "1").current_dir(law.dir.path());
            // SAFETY: `setrlimit` is async-signal-safe and caps only this child.
            unsafe {
                command.pre_exec(move || {
                    let no_core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                    let cap = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
                    if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0
                        || libc::setrlimit(resource, &cap) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let out = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(stdout, lines, "{engine} {mode}: {out:?}\n{stderr}");
            assert_eq!(out.status.code(), Some(102), "{engine} {mode}: {out:?}\n{stderr}");
            assert_eq!(stderr, "run stop: HostMemory\n", "{engine} {mode}: {out:?}");
        }
    }
}

/// FW17 and FW27: a test that reaches only contained code is recorded and
/// replays `Sealed` (FFI-14). FW17's test calls `spin`, `fill`, `grow` under
/// `bound bytes` and `trap` inside `isolate`; FW27's overflows the guest
/// stack inside `isolate` under `bound steps 10⁹` beside a task that prints, on
/// 2 workers, so its slice ends must be the recording's. A contained test
/// takes `Foreign[Box]`, so a second run with the same cache runs it again
/// (PROG-4). A test that also reaches a native module is refused as FL7 is.
/// The package's test configuration grants `Foreign=…Box` and the command
/// line does not: a contained interface needs no `--grant` and writes no
/// `foreign:` line (FFI-14 Grants).
#[test]
#[ignore = "explicit native integration target"]
fn contained_tests_record_and_replay() {
    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/wasm_laws");
    for file in ["laws.hirsel", "native.hirsel", "box.wasm"] {
        law.write(&format!("wasm_laws/{file}"), std::fs::read(fixture.join(file)).unwrap());
    }
    let manifest = std::fs::read_to_string(fixture.join("hirsel.toml")).unwrap();
    law.write("wasm_laws/hirsel.toml", format!(
        "{}\n[foreign.other]\nsymbols = \"hwo_\"\narchives = []\n\n[tests]\nlimit = 268435456\n{}\n",
        manifest.replace("\"main.hirsel\" = \"main\"", "\"main.hirsel\" = \"main\"\n\"other.hirsel\" = \"other\""),
        r#"grants = ["Foreign=hirsel.ai/wasm_laws@1/laws/Box", "Foreign=hirsel.ai/wasm_laws@1/laws/Other", "Tasks", "Stdout"]"#,
    ));
    law.write("wasm_laws/other.hirsel", "pub fn poke(f: Foreign[laws.Other]) -> i32\n");
    law.write("wasm_laws/laws.hirsel", format!(
        "{}\npub opaque type Other {{}}\n",
        std::fs::read_to_string(fixture.join("laws.hirsel")).unwrap(),
    ));
    law.write("wasm_laws/main.hirsel", CONTAINED_REPLAY);
    let execution = ContainedExecution::new(&law);
    let record = match &execution.runner {
        crate::Runner::Native => env_record_runtime(),
        crate::Runner::Qemu { .. } => PathBuf::from(std::env::var_os("HIRSEL_CONTAINED_RECORD_RUNTIME").unwrap()),
    };
    let test = |args: &[&str]| {
        let runtime = if args.contains(&"--replay") { &record } else { &execution.runtime };
        let mut argv = [
            "test", "wasm_laws", "--json", "--target", execution.target,
            "--libraries", law.libraries.to_str().unwrap(),
            "--store", law.path("store").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(),
            "--grant", "Foreign=hirsel.ai/wasm_laws@1/laws/Other",
        ].into_iter().chain(execution.link_args(runtime)).map(str::to_owned).collect::<Vec<_>>();
        if let crate::Runner::Qemu { emulator, sysroot } = &execution.runner {
            argv.extend(["--emulator", emulator.to_str().unwrap(), "--sysroot", sysroot.to_str().unwrap()].map(str::to_owned));
        }
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        let mut command = process::tape_request_command(&argv, law.dir.path());
        command.env("HIRSEL_UNTIED", "2");
        process::tape_request_output(command)
    };
    let replay = ["--replay", "--expect-verdict", "Sealed"];
    let engine = ContainedExecution::engines()[0];
    let (code, out, err) = test(&["--backend", engine, "--exact", "hirsel.ai/wasm_laws@1/main/fw17_contained_replays"]);
    assert_eq!(code, Some(0), "unreplayed: {out}\n{err}");
    assert!(!err.lines().any(|line| line.starts_with("foreign: ")), "{err}");
    for (engine, name) in ContainedExecution::engines()
        .iter()
        .flat_map(|&e| ["fw17_contained_replays", "fw27_overflow_replays"].map(|n| (e, n)))
    {
        for _ in 0..2 {
            let exact = format!("hirsel.ai/wasm_laws@1/main/{name}");
            let (code, out, err) = test(&[&replay[..], &["--backend", engine, "--exact", &exact]].concat());
            assert_eq!(code, Some(0), "{engine} {name}: {out}\n{err}");
            let report: serde_json::Value = serde_json::from_str(&out).unwrap();
            let tests = report["tests"].as_array().unwrap();
            assert_eq!(tests.len(), 1, "{report}");
            assert_eq!((&tests[0]["source"], &tests[0]["passed"]), (&"executed".into(), &true.into()), "{report}");
        }
    }
    let (code, out, err) = test(&[&replay[..], &["--backend", engine, "--exact", "hirsel.ai/wasm_laws@1/main/fl7_both"]].concat());
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(out.contains("refused: FFI-8: a recording build cannot reach foreign code: hirsel.ai/wasm_laws@1/main/fl7_both reaches hirsel.ai/wasm_laws@1/other/poke"), "{out}\n{err}");
}

const CONTAINED_REPLAY: &str = r#"
fn say(out: Stdout, text: String) waits {
    result.unwrap(stdout.write(out, text))
    result.unwrap(stdout.write(out, "\n"))
}

fn trapped[R](take outcome: Result[Result[R, Causes], Capacity]) -> bool {
    match take outcome {
        Ok(Error(_)) => true,
        _ => false,
    }
}

@test
fn fw17_contained_replays(f: Foreign[laws.Box], out: Stdout) -> Result[(), ()] waits {
    let mut s = string.from("FW17 ")
    string.push_u64(mut s, native.spin(f, 1000))
    let mut b: Bytes = list.with_capacity(4)
    list.fill(mut b, 4, 0)
    string.push_char(mut s, ' ')
    string.push_u64(mut s, native.fill(f, mut b))
    let grown = match (bound bytes 1048576 {
        native.grow(f, 1)
    }) {
        Ok(old) => old,
        Error(_) => return Error(()),
    }
    if grown < 0 {
        return Error(())
    }
    if !trapped(isolate(fn() -> i32 {
        native.trap(f, 7)
    })) {
        return Error(())
    }
    say(out, s)
    Ok(())
}

fn printer(take out: Stdout) -> u64 waits {
    say(out, "FW27 beside")
    0
}

@test
fn fw27_overflow_replays(f: Foreign[laws.Box], tasks: Tasks, out: Stdout) -> Result[(), ()] waits {
    let child = task.spawn(tasks, bytes 1048576, out, printer)
    let overflowed = match (bound steps 1000000000 {
        trapped(isolate(fn() -> u32 {
            native.recurse(f, 10000000)
        }))
    }) {
        Ok(t) => t,
        Error(_) => false,
    }
    let _ = task.join(take child)
    if overflowed { Ok(()) } else { Error(()) }
}

@test
fn fl7_both(f: Foreign[laws.Box], g: Foreign[laws.Other]) -> Result[(), ()] {
    let _ = native.count(f)
    let _ = other.poke(g)
    Ok(())
}
"#;

/// FW18: a contained function's address holds its module's pin (FFI-16), so
/// one changed byte of `box.wasm` (a letter of its `name` section, which no
/// export reads) and its pin change `caller`'s revision and leave `plain`'s,
/// which reaches no foreign code. Each recipe builds and runs from the code
/// store with the package deleted; `count` returns 1, the first entry of
/// `box.c`'s counter. A recipe whose module object is gone is `Missing`.
#[test]
fn contained_recipes_pin_their_module() {
    use crate::code_store::{CodeStore, ObjectKind};
    use sha2::{Digest, Sha256};

    let fixture = |file: &str| std::fs::read(PackageLaw::new().libraries.join("programs/wasm_laws").join(file)).unwrap();
    let original = fixture("box.wasm");
    let at = original.windows(4).position(|w| w == b"bump").unwrap() + 3;
    let mut changed = original.clone();
    changed[at] = b'q';
    let manifest = String::from_utf8(fixture("hirsel.toml")).unwrap();
    let pin = |bytes: &[u8]| ContentHash(Sha256::digest(bytes).into());
    for &engine in ContainedExecution::engines() {
        let law = PackageLaw::new();
        let execution = ContainedExecution::new(&law);
        let code = law.path("code-store");
        let store = CodeStore::new(&code);
        let build = |root: &str| {
            let request = [
                "build", "--recipe", root, "-o", "sealed-program",
                "--code-store", code.to_str().unwrap(),
                "--store", law.path("compiler-store").to_str().unwrap(),
                "--package-store", law.path("packages").to_str().unwrap(),
            ].into_iter().chain(execution.link_args(&execution.runtime)).map(str::to_owned).collect::<Vec<_>>();
            let mut command = process::tape_request_command(&request, law.dir.path());
            command.env("XDG_CACHE_HOME", law.path("cache"));
            process::tape_request_output(command)
        };
        let mut runs = Vec::new();
        for module in [&original, &changed] {
            for file in ["laws.hirsel", "native.hirsel"] {
                law.write(&format!("wasm_laws/{file}"), fixture(file));
            }
            law.write("wasm_laws/box.wasm", module);
            law.write("wasm_laws/hirsel.toml", manifest.replace(&pin(&original).to_string(), &pin(module).to_string()));
            law.write("wasm_laws/main.hirsel", CONTAINED_RECIPE);
            let root = PackageLaw::pass(law.run(&[
                "publish", "wasm_laws", "--backend", engine, "--target", execution.target,
                "--grant", "Foreign=hirsel.ai/wasm_laws@1/laws/Box", "--grant", "Stdout",
            ])).trim().to_owned();
            std::fs::remove_dir_all(law.path("wasm_laws")).unwrap();
            PackageLaw::pass(build(&root));
            let run = execution.runner.command(&law.path("sealed-program")).output().unwrap();
            assert!(run.status.success(), "{engine}: {run:?}");
            assert_eq!(run.stdout.len(), 64, "{engine}: two revisions");
            runs.push((root, run.stdout));
        }
        let [(first, a), (_, b)] = &runs[..] else { unreachable!() };
        assert_ne!(a[..32], b[..32], "{engine}: the pin is in caller's address");
        assert_eq!(a[32..], b[32..], "{engine}: plain reaches no module");
        std::fs::remove_file(store.path(ObjectKind::ForeignArchive, pin(&original))).unwrap();
        let (status, _, error) = build(first);
        assert_eq!(status, Some(2), "{engine}: {error}");
        assert!(error.contains(&format!("Missing({})", pin(&original))), "{engine}: {error}");
    }
}

const CONTAINED_RECIPE: &str = "\
fn caller(f: Foreign[laws.Box]) -> u64 { native.count(f) }
fn plain() -> u64 { 9 }
fn put[F](out: Stdout, s: StaticFn[F]) -> bool waits {
    let Some(r) = code.revision(s) else { return false };
    result.unwrap(stdout.write_bytes(out, r));
    true
}
pub fn main(f: Foreign[laws.Box], out: Stdout) -> Result[(), Exit] waits {
    if put(out, code.describe(caller)) && put(out, code.describe(plain)) && plain() == 9 && caller(f) == 1 {
        Ok(())
    } else {
        Error(Exit(status: 2u8))
    }
}
";
