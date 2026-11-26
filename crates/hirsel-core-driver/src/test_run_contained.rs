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

    // FW20: the fixture module is admitted, and the key is refused as unbuilt;
    // `native` is not a containment.
    law.write("wasm_laws/box.wasm", fs::read(fixture.join("box.wasm")).unwrap());
    law.write("wasm_laws/hirsel.toml", original.as_str());
    for engine in ["cranelift", "llvm"] {
        refused(engine, &line("FFI-10: containment = \"wasm\" is not built yet".into()), "FW20 unbuilt");
    }
    law.write("wasm_laws/box.wasm", b"abc");
    manifest(&format!("containment = \"native\"\nmodule = {{ file = \"box.wasm\", sha256 = \"{ABC}\" }}"));
    refused("cranelift", &|line: &str| line.ends_with("hirsel.toml: foreign.native: containment: expected \"wasm\""), "FW20");
}
