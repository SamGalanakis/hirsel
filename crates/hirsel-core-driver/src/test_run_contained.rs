// Contained foreign laws (docs/spec/foreign.md, "Contained laws"). The refusal
// modules are written here byte by byte from the WebAssembly binary format, so
// each one holds exactly the construct its law names.

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
    const MODULE: &str = "FFI-11: module box.wasm of wasm_laws@1/native";
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
    let options = native_options(Backend::Cranelift, law.dir.path());
    let runtime = options.runtime.canonicalize().unwrap();
    let build = |engine: &str| {
        let request = [
            "build", "wasm_laws", "--backend", engine,
            "--libraries", law.libraries.to_str().unwrap(), "--runtime", runtime.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", "image",
            "--grant", "Foreign=wasm_laws@1/laws/Box",
        ].map(str::to_owned);
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

    for engine in ["cranelift", "llvm"] {
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
    for engine in ["cranelift", "llvm"] {
        let (status, output, error) = build(engine);
        assert_eq!(status, Some(0), "FW20 builds\n{output}\n{error}");
        assert!(law.path("image").exists(), "FW20: no image");
        fs::remove_file(law.path("image")).unwrap();
    }
    law.write("wasm_laws/box.wasm", b"abc");
    manifest(&format!("containment = \"native\"\nmodule = {{ file = \"box.wasm\", sha256 = \"{ABC}\" }}"));
    refused("cranelift", &|line: &str| line.ends_with("hirsel.toml: foreign.native: containment: expected \"wasm\""), "FW20");
}

/// FW3, FW5 to FW16, FW21 to FW26 and FW28 on `box.wasm`, each observation
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
    let options = native_options(Backend::Cranelift, law.dir.path());
    let runtime = options.runtime.canonicalize().unwrap();
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
        ("tasks", ["FW14 one 123", "FW14 two 123", "FW25 one 0", "FW25 two 0"].map(str::to_owned).to_vec()),
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
    for engine in ["cranelift", "llvm"] {
        let image = law.path(&format!("image-{engine}"));
        let request = [
            "build", "wasm_laws", "--backend", engine,
            "--libraries", law.libraries.to_str().unwrap(), "--runtime", runtime.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", image.to_str().unwrap(),
            "--grant", "Foreign=wasm_laws@1/laws/Box", "--grant", "Tasks", "--grant", "Stdout",
            "--limit", "268435456",
        ].map(str::to_owned);
        let (status, output, error) = process::tape_request(&request, law.dir.path());
        assert_eq!(status, Some(0), "{engine}: build\n{output}\n{error}");
        let run = |mode: &str, threads: &str| {
            Command::new("timeout").args(["120s", image.to_str().unwrap(), mode])
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
        assert_eq!(out.status.code(), Some(102), "{engine} signals: {:?}\n{stderr}", out.status);
        assert!(stderr.contains("run stop: Stack"), "{engine} signals: {stderr}");
        assert!(!stdout.is_empty() && stdout.lines().all(|line| line == trap("memory out of bounds")), "{engine} signals: {stdout}");
    }
}
