//! The supervisor's laws, against workers that are shell scripts: each does one thing a
//! hostile compile makes a real worker do. The expected refusals are PACK-3's table.

use super::*;
use hirsel_mod::{Header, Image};
use std::os::unix::fs::PermissionsExt;

const FILES: &[(&str, &[u8])] = &[("main.hirsel", b"pub fn main() {}")];

fn request(target: Target) -> Request<'static> {
    Request {
        files: FILES,
        target,
        backend: Backend::Cranelift,
        policy: 0,
    }
}

fn caps() -> Caps {
    Caps {
        cpu_seconds: 5,
        memory_bytes: 64 << 20,
        wall_ms: 10_000,
        input_bytes: 4096,
        output_bytes: 4096,
    }
}

/// A service whose worker runs `script` with the worker's arguments (`${10}` is the output).
fn service(dir: &Path, script: &str, caps: Caps, workers: usize, queue: usize) -> Service {
    let worker = dir.join("worker");
    std::fs::write(&worker, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
    Service::new(worker, dir.join("scratch"), caps, workers, queue)
}

fn refusal(script: &str, caps: Caps) -> Refusal {
    let dir = tempfile::tempdir().unwrap();
    service(dir.path(), script, caps, 1, 0)
        .compile(&request(Target::X86_64Linux))
        .unwrap_err()
}

fn module(target: Target) -> Vec<u8> {
    Image {
        header: Header {
            target,
            backend: Backend::Cranelift,
            policy: 0,
            abi: hirsel_mod::abi(),
            compiler: ContentHash([2; 32]),
            coreir: ContentHash([3; 32]),
            text_align: 64,
            data_align: 64,
        },
        text: vec![0xc3],
        data: vec![],
        imports: vec![],
        text_fixups: vec![],
        data_fixups: vec![],
        entries: vec![],
        spans: vec![],
        cfi: vec![],
        sites: vec![],
        names: vec![],
    }
    .encode()
}

#[test]
fn a_worker_that_breaks_a_cap_ends_as_that_limit() {
    let spin = Caps {
        cpu_seconds: 1,
        ..caps()
    };
    assert_eq!(
        refusal("while :; do :; done", spin),
        Refusal::Limit(Limit::Cpu)
    );

    // The script's child holds the worker's standard error too: the whole group ends
    // at the deadline, long before the sleep would.
    let started = Instant::now();
    let brief = Caps {
        wall_ms: 200,
        ..caps()
    };
    assert_eq!(refusal("/bin/sleep 60", brief), Refusal::Limit(Limit::Wall));
    assert!(started.elapsed() < Duration::from_secs(20));

    assert_eq!(refusal("exit 71", caps()), Refusal::Limit(Limit::Memory));
    // One byte past the cap is the largest file the worker can write.
    assert_eq!(
        refusal("/usr/bin/head -c 4097 /dev/zero > \"${10}\"", caps()),
        Refusal::Limit(Limit::Output)
    );
    assert_eq!(
        refusal(
            "exec /usr/bin/head -c 1000000 /dev/zero > \"${10}\"",
            caps()
        ),
        Refusal::Limit(Limit::Output)
    );

    let dir = tempfile::tempdir().unwrap();
    let large = [("main.hirsel", &[b' '; 4096][..])];
    let never = Service::new(
        dir.path().join("absent"),
        dir.path().join("scratch"),
        caps(),
        1,
        0,
    );
    let over = Request {
        files: &large,
        ..request(Target::X86_64Linux)
    };
    assert_eq!(never.compile(&over), Err(Refusal::Limit(Limit::Input)));
}

#[test]
fn a_worker_s_end_is_classified_and_its_module_is_checked() {
    assert_eq!(
        refusal(
            "echo '{\"Unsupported\":\"the release reaches hirsel_core_exec_spawn\"}'; exit 1",
            caps()
        ),
        Refusal::Unsupported("the release reaches hirsel_core_exec_spawn".into())
    );
    assert_eq!(
        refusal(
            "echo '\"Rejected\"'; echo 'main.hirsel:1:1: no' >&2; exit 1",
            caps()
        ),
        Refusal::Rejected("main.hirsel:1:1: no\n".into())
    );
    assert_eq!(
        refusal("echo 'hirsel: no runtime' >&2; exit 1", caps()),
        Refusal::Unavailable("hirsel: no runtime\n".into())
    );
    assert!(matches!(
        refusal("kill -SEGV $$", caps()),
        Refusal::Unavailable(_)
    ));
    assert!(matches!(
        refusal("echo junk > \"${10}\"", caps()),
        Refusal::Unavailable(_)
    ));
    assert!(matches!(refusal("exit 0", caps()), Refusal::Unavailable(_)));

    let dir = tempfile::tempdir().unwrap();
    let bytes = module(Target::X86_64Linux);
    std::fs::write(dir.path().join("module"), &bytes).unwrap();
    let copy = format!(
        "/bin/cp {}/module \"${{10}}\" && echo '\"Built\"'",
        dir.path().display()
    );
    let service = service(dir.path(), &copy, caps(), 1, 0);
    let built = service.compile(&request(Target::X86_64Linux)).unwrap();
    assert_eq!(built.bytes, bytes);
    assert_eq!(built.digest, hirsel_mod::digest(&bytes));
    assert_eq!(built.key, Module::parse(&bytes).unwrap().header.key());
    // The same reply to a request for another target is not that request's module.
    let other = service.compile(&request(Target::Aarch64Linux));
    assert!(matches!(other, Err(Refusal::Unavailable(_))));
    // A source path cannot leave the job's package directory.
    for name in ["../escape", "/escape", "a/../../escape", ""] {
        let files = [(name, &b"x"[..])];
        let leaving = Request {
            files: &files,
            ..request(Target::X86_64Linux)
        };
        assert!(
            matches!(service.compile(&leaving), Err(Refusal::Rejected(_))),
            "{name}"
        );
    }
    // Every job's directory is gone once it is answered.
    let left: Vec<_> = std::fs::read_dir(dir.path().join("scratch"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_full_queue_answers_busy_and_a_queued_request_runs_later() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("started");
    let script = format!(": >> {}; /bin/sleep 1; exit 71", mark.display());
    for (queue, second) in [(0, Refusal::Busy), (1, Refusal::Limit(Limit::Memory))] {
        let _ = std::fs::remove_file(&mark);
        let service = service(dir.path(), &script, caps(), 1, queue);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| service.compile(&request(Target::X86_64Linux)));
            while !mark.exists() {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(service.compile(&request(Target::X86_64Linux)), Err(second));
            assert_eq!(first.join().unwrap(), Err(Refusal::Limit(Limit::Memory)));
        });
    }
}

// ---------------------------------------------------------------------------
// G22: Registry check_library boundary, graph/history rules, and confinement
// ---------------------------------------------------------------------------

enum TestBinding {
    Toolchain,
    Exact(ContentHash),
    Floor {
        tested: ContentHash,
        minor: u32,
        patch: u32,
    },
}

fn c_leb(out: &mut Vec<u8>, mut value: usize) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

fn c_frame(out: &mut Vec<u8>, bytes: &[u8]) {
    c_leb(out, bytes.len());
    out.extend_from_slice(bytes);
}

/// Independent Phase 0 canonical C v3 encoder (`docs/spec/packages.md` §1).
fn encode_c_v3(
    name: &str,
    (major, minor, patch): (u32, u32, u32),
    pre: &str,
    bindings: &[(&str, &str, u32, TestBinding)],
    files: &[(&str, &[&str], &[u8])],
) -> (ContentHash, Vec<u8>) {
    let mut out = Vec::new();
    c_leb(&mut out, 3); // C v3
    c_frame(&mut out, name.as_bytes());
    for n in [major, minor, patch] {
        c_leb(&mut out, n as usize);
    }
    c_frame(&mut out, pre.as_bytes());
    c_frame(&mut out, b"charge-v1");
    c_leb(&mut out, bindings.len());
    for (alias, dep_name, dep_major, binding) in bindings {
        c_frame(&mut out, alias.as_bytes());
        c_frame(&mut out, dep_name.as_bytes());
        c_leb(&mut out, *dep_major as usize);
        match binding {
            TestBinding::Toolchain => c_leb(&mut out, 0),
            TestBinding::Exact(root) => {
                c_leb(&mut out, 1);
                out.extend_from_slice(&root.0);
            }
            TestBinding::Floor {
                tested,
                minor,
                patch,
            } => {
                c_leb(&mut out, 2);
                out.extend_from_slice(&tested.0);
                c_leb(&mut out, *minor as usize);
                c_leb(&mut out, *patch as usize);
            }
        }
    }
    c_leb(&mut out, files.len());
    for (path, module, bytes) in files {
        c_frame(&mut out, path.as_bytes());
        c_leb(&mut out, module.len());
        for part in *module {
            c_frame(&mut out, part.as_bytes());
        }
        c_frame(&mut out, bytes);
    }
    c_leb(&mut out, 0); // foreign modules
    let root = ContentHash::of_parts("hirsel.library-root.v1", [&out[..]]);
    (root, out)
}

/// Build a well-formed C v3 payload whose total encoded length is `exact_len` bytes,
/// carrying a pinned contained WebAssembly foreign module row sized to hit `exact_len`.
fn encode_c_v3_exact_size(name: &str, exact_len: usize) -> (ContentHash, Vec<u8>) {
    use sha2::{Digest, Sha256};

    let (_, mut prefix) = encode_c_v3(
        name,
        (1, 0, 0),
        "",
        &[],
        &[
            (
                "ext.hirsel",
                &["ext"],
                b"pub fn noop(f: Foreign[lib.Box]) -> u64\n",
            ),
            (
                "lib.hirsel",
                &["lib"],
                b"pub opaque type Box {}\npub fn ok() -> u64 { 1 }\n",
            ),
        ],
    );
    prefix.pop(); // replace 0 foreign-module count with 1 contained wasm module
    c_leb(&mut prefix, 1);
    c_leb(&mut prefix, 1);
    c_frame(&mut prefix, b"ext");
    c_frame(&mut prefix, b"ext_");
    c_leb(&mut prefix, 1);
    c_frame(&mut prefix, b"ext.wasm");
    let mut wasm_len = exact_len - prefix.len() - 40;
    loop {
        let mut wasm = vec![0u8; wasm_len];
        wasm[..8].copy_from_slice(b"\0asm\x01\x00\x00\x00");
        let pin: [u8; 32] = Sha256::digest(&wasm).into();
        let mut out = Vec::with_capacity(exact_len + 16);
        out.extend_from_slice(&prefix);
        out.extend_from_slice(&pin);
        c_frame(&mut out, &wasm);
        if out.len() == exact_len {
            let root = ContentHash::of_parts("hirsel.library-root.v1", [&out[..]]);
            return (root, out);
        }
        wasm_len = if out.len() > exact_len {
            wasm_len - (out.len() - exact_len)
        } else {
            wasm_len + (exact_len - out.len())
        };
    }
}

fn real_worker_path() -> PathBuf {
    let raw = std::env::var_os("HIRSEL_COMPILER_WORKER")
        .expect("HIRSEL_COMPILER_WORKER must point to hirsel driver binary");
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        path
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
            .canonicalize()
            .expect("resolve HIRSEL_COMPILER_WORKER")
    }
}

fn assert_scratch_empty(scratch: &Path) {
    if !scratch.exists() {
        return;
    }
    let left: Vec<_> = std::fs::read_dir(scratch)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        left.is_empty(),
        "zero bytes may persist in scratch after check_library, found {left:?}"
    );
}

#[test]
fn g22_check_library_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = dir.path().join("scratch");
    let worker = real_worker_path();
    let real_service = Service::new(worker, scratch.clone(), Caps::HOST, 1, 0);

    fn req<'a>(
        root: ContentHash,
        content: &'a [u8],
        closure: &'a [LibraryEntry<'a>],
        history: &'a [HistoryRow],
    ) -> CheckLibraryRequest<'a> {
        CheckLibraryRequest {
            root,
            content,
            closure,
            history,
        }
    }
    fn entry<'a>(root: ContentHash, content: &'a [u8]) -> LibraryEntry<'a> {
        LibraryEntry { root, content }
    }
    fn hist(identity: &str, version: &str, root: ContentHash) -> HistoryRow {
        HistoryRow {
            origin: "https://pkg.hirsel.ai".into(),
            identity: identity.into(),
            version: version.into(),
            root,
        }
    }

    // -----------------------------------------------------------------------
    // 1. Pre-spawn input caps refuse Limit(Input) on all hosts; hosts without
    //    OS aggregate memory confinement fail closed with Unavailable before
    //    spawning. On supported hosts, exact 64 MiB raw C reaches decoding.
    // -----------------------------------------------------------------------
    const SIXTY_FOUR_MIB: usize = 64 << 20;
    let absent_service = Service::new(
        dir.path().join("never-spawned"),
        dir.path().join("never-scratch"),
        Caps::HOST,
        1,
        0,
    );
    let over_64_bytes = vec![0u8; SIXTY_FOUR_MIB + 1];
    assert_eq!(
        absent_service.check_library(&req(ContentHash([0; 32]), &over_64_bytes, &[], &[])),
        Err(Refusal::Limit(Limit::Input))
    );
    drop(over_64_bytes);

    // >4,096 packages and >64 MiB history metadata also refuse Limit(Input) before spawning.
    let tiny = [0u8; 1];
    let many: Vec<LibraryEntry<'_>> = (0..4096)
        .map(|_| entry(ContentHash([1; 32]), &tiny))
        .collect();
    assert_eq!(
        absent_service.check_library(&req(ContentHash([0; 32]), &tiny, &many, &[])),
        Err(Refusal::Limit(Limit::Input))
    );
    let huge_origin = format!("https://{}.example", "a".repeat(32 << 20));
    let huge_history = [
        HistoryRow {
            origin: huge_origin.clone(),
            identity: "github.com/acme/a@1".into(),
            version: "1.0.0".into(),
            root: ContentHash([2; 32]),
        },
        HistoryRow {
            origin: huge_origin,
            identity: "github.com/acme/b@1".into(),
            version: "1.0.0".into(),
            root: ContentHash([3; 32]),
        },
    ];
    assert_eq!(
        absent_service.check_library(&req(ContentHash([0; 32]), &tiny, &[], &huge_history)),
        Err(Refusal::Limit(Limit::Input))
    );
    assert!(!dir.path().join("never-scratch").exists());

    if let Some(reason) = host::missing_confinement() {
        assert!(
            std::env::var_os("HIRSEL_TEST_REQUIRE_CGROUP").is_none(),
            "{reason}"
        );
        assert_eq!(
            real_service.check_library(&req(ContentHash([0; 32]), &tiny, &[], &[])),
            Err(Refusal::Unavailable(reason.into()))
        );
        assert_scratch_empty(&scratch);
        return;
    }

    let (exact_64_root, exact_64_bytes) =
        encode_c_v3_exact_size("github.com/acme/exact64", SIXTY_FOUR_MIB);
    assert_eq!(exact_64_bytes.len(), SIXTY_FOUR_MIB);
    let exact_64_result =
        real_service.check_library(&req(exact_64_root, &exact_64_bytes, &[], &[]));
    drop(exact_64_bytes);
    assert_scratch_empty(&scratch);

    // -----------------------------------------------------------------------
    // 2 & 3. Independently constructed valid graph + refusal matrix.
    // -----------------------------------------------------------------------
    let (leaf_root, leaf_bytes) = encode_c_v3(
        "github.com/acme/leaf",
        (1, 2, 3),
        "",
        &[],
        &[(
            "leaf.hirsel",
            &["leaf"],
            b"pub fn add(a: u64, b: u64) -> u64 { a + b }\n",
        )],
    );
    let (exact_root, exact_bytes) = encode_c_v3(
        "github.com/acme/exactdep",
        (0, 1, 0),
        "alpha.1",
        &[],
        &[(
            "exactdep.hirsel",
            &["exactdep"],
            b"pub fn tag() -> u64 { 7 }\n",
        )],
    );
    let (head_root, head_bytes) = encode_c_v3(
        "github.com/acme/head",
        (2, 0, 0),
        "",
        &[
            (
                "dep",
                "github.com/acme/exactdep",
                0,
                TestBinding::Exact(exact_root),
            ),
            (
                "leaf",
                "github.com/acme/leaf",
                1,
                TestBinding::Floor {
                    tested: leaf_root,
                    minor: 2,
                    patch: 3,
                },
            ),
        ],
        &[(
            "head.hirsel",
            &["head"],
            b"/// Combine leaf and dep.\npub fn compute(x: u64) -> u64 {\n    leaf.add(x, dep.tag())\n}\n\npub fn with_clock(_c: Clock) -> u64 {\n    1\n}\n",
        )],
    );
    let valid_closure = [
        entry(exact_root, &exact_bytes),
        entry(leaf_root, &leaf_bytes),
    ];
    let valid_history = [
        hist("github.com/acme/leaf@1", "1.2.3", leaf_root),
        hist("github.com/acme/exactdep@0", "0.1.0-alpha.1", exact_root),
    ];

    // Valid check returns exact CheckedLibrary.
    let checked = real_service
        .check_library(&req(head_root, &head_bytes, &valid_closure, &valid_history))
        .expect("valid head + closure + verified history must check");
    assert_eq!(checked.root, head_root.to_string());
    assert_eq!(
        checked.header,
        CheckedHeader {
            identity: "github.com/acme/head@2".into(),
            version: "2.0.0".into(),
            charge: "charge-v1".into(),
        }
    );
    // Bindings must be returned in canonical C order (sorted by alias).
    assert_eq!(
        checked.bindings,
        vec![
            CheckedBinding::Exact {
                alias: "dep".into(),
                id: "github.com/acme/exactdep@0".into(),
                tested_root: exact_root.to_string(),
            },
            CheckedBinding::Floor {
                alias: "leaf".into(),
                id: "github.com/acme/leaf@1".into(),
                tested_root: leaf_root.to_string(),
                bound_version: "1.2.3".into(),
            },
        ]
    );
    // InterfaceV1 must match unfiltered inspect output.
    assert_eq!(checked.interface.version, 1);
    assert_eq!(
        checked.interface.package,
        hirsel_core_ids::PackageId {
            name: "github.com/acme/head".into(),
            version: "2".into(),
        }
    );
    assert_eq!(checked.interface.modules.len(), 1);
    let head_mod = &checked.interface.modules[0];
    assert_eq!(head_mod.path, vec!["head"]);
    assert!(!head_mod.extends_core);
    assert_eq!(head_mod.items.len(), 2);
    assert_eq!(head_mod.items[0].name, "compute");
    assert_eq!(head_mod.items[0].kind, "function");
    assert_eq!(
        head_mod.items[0].definition,
        "github.com/acme/head@2/head/compute"
    );
    assert_eq!(
        head_mod.items[0].doc.as_deref(),
        Some("Combine leaf and dep.")
    );
    assert_eq!(head_mod.items[1].name, "with_clock");
    assert_eq!(head_mod.items[1].doc, None);
    // Authority Card must reflect §11 CoreIR analysis.
    assert_eq!(
        checked.authority.carried.get("head/with_clock"),
        Some(&CarriedAuthority {
            takes: vec!["Clock".into()],
            returns: vec![],
        })
    );
    assert!(checked.authority.reached.contains_key("head/compute"));
    assert!(checked.authority.reached.contains_key("head/with_clock"));
    assert_eq!(
        checked.authority.verdict,
        AuthorityVerdict::Status("conditional".into())
    );
    // RFC 8785 canonical JSON round-trips with no trailing newline.
    let canonical = checked.to_canonical_json().unwrap();
    assert!(!canonical.ends_with('\n'));
    assert_eq!(checked.facts_bytes, canonical.as_bytes());
    assert_eq!(
        checked.facts_digest,
        ContentHash(<sha2::Sha256 as sha2::Digest>::digest(&checked.facts_bytes).into())
            .to_string()
    );
    assert!(parse_digest(&checked.compiler_id).is_some());
    assert_scratch_empty(&scratch);

    let assert_rejected = |r: CheckLibraryRequest<'_>| {
        assert!(matches!(
            real_service.check_library(&r),
            Err(Refusal::Rejected(_))
        ));
    };

    // 2a. Wrong head root & wrong closure root refuse.
    let mut bad_head_root = head_root;
    bad_head_root.0[0] ^= 1;
    assert_rejected(req(
        bad_head_root,
        &head_bytes,
        &valid_closure,
        &valid_history,
    ));
    let mut bad_leaf_root = leaf_root;
    bad_leaf_root.0[0] ^= 1;
    let bad_closure = [valid_closure[0], entry(bad_leaf_root, &leaf_bytes)];
    assert_rejected(req(head_root, &head_bytes, &bad_closure, &valid_history));

    // 2b. Missing logged floor history refuses.
    assert_rejected(req(
        head_root,
        &head_bytes,
        &valid_closure,
        &valid_history[1..],
    ));

    // 2c. False child version: header claims 1.2.4 while floor binds 1.2.3, and
    //     header claims 1.2.3 while verified history says 1.2.2.
    let (false_leaf_root, false_leaf_bytes) = encode_c_v3(
        "github.com/acme/leaf",
        (1, 2, 4),
        "",
        &[],
        &[(
            "leaf.hirsel",
            &["leaf"],
            b"pub fn add(a: u64, b: u64) -> u64 { a + b }\n",
        )],
    );
    let (head_fv_root, head_fv_bytes) = encode_c_v3(
        "github.com/acme/head",
        (2, 0, 0),
        "",
        &[(
            "leaf",
            "github.com/acme/leaf",
            1,
            TestBinding::Floor {
                tested: false_leaf_root,
                minor: 2,
                patch: 3,
            },
        )],
        &[(
            "head.hirsel",
            &["head"],
            b"pub fn f(x: u64) -> u64 { leaf.add(x, 1) }\n",
        )],
    );
    assert_rejected(req(
        head_fv_root,
        &head_fv_bytes,
        &[entry(false_leaf_root, &false_leaf_bytes)],
        &[hist("github.com/acme/leaf@1", "1.2.3", false_leaf_root)],
    ));
    assert_rejected(req(
        head_root,
        &head_bytes,
        &valid_closure,
        &[hist("github.com/acme/leaf@1", "1.2.2", leaf_root)],
    ));

    // 2d. Mismatched exact-child history, missing closure entry, and extra closure entry refuse.
    let bad_exact_hist = [
        valid_history[0].clone(),
        hist("github.com/acme/exactdep@0", "0.1.1", exact_root),
    ];
    assert_rejected(req(head_root, &head_bytes, &valid_closure, &bad_exact_hist));
    assert_rejected(req(
        head_root,
        &head_bytes,
        &valid_closure[1..],
        &valid_history[..1],
    ));
    let (extra_root, extra_bytes) = encode_c_v3(
        "github.com/acme/extra",
        (1, 0, 0),
        "",
        &[],
        &[("extra.hirsel", &["extra"], b"pub fn z() -> u64 { 0 }\n")],
    );
    let extra_closure = [
        valid_closure[0],
        valid_closure[1],
        entry(extra_root, &extra_bytes),
    ];
    assert_rejected(req(head_root, &head_bytes, &extra_closure, &valid_history));

    // 2e. Dependency cycle (A@1 -> B@1 -> A@1) refuses cleanly.
    let (a_leaf_root, a_leaf_bytes) = encode_c_v3(
        "github.com/acme/cyca",
        (1, 0, 0),
        "",
        &[],
        &[("cyca.hirsel", &["cyca"], b"pub fn base() -> u64 { 1 }\n")],
    );
    let (b_root, b_bytes) = encode_c_v3(
        "github.com/acme/cycb",
        (1, 0, 0),
        "",
        &[(
            "a",
            "github.com/acme/cyca",
            1,
            TestBinding::Exact(a_leaf_root),
        )],
        &[(
            "cycb.hirsel",
            &["cycb"],
            b"pub fn mid() -> u64 { a.base() }\n",
        )],
    );
    let (a_head_root, a_head_bytes) = encode_c_v3(
        "github.com/acme/cyca",
        (1, 0, 1),
        "",
        &[("b", "github.com/acme/cycb", 1, TestBinding::Exact(b_root))],
        &[(
            "cyca.hirsel",
            &["cyca"],
            b"pub fn top() -> u64 { b.mid() }\n",
        )],
    );
    let cyc_closure = [entry(b_root, &b_bytes), entry(a_leaf_root, &a_leaf_bytes)];
    let cycle_err = real_service
        .check_library(&req(a_head_root, &a_head_bytes, &cyc_closure, &[]))
        .unwrap_err();
    assert!(
        matches!(&cycle_err, Refusal::Rejected(msg) if msg.contains("cycle")),
        "expected cycle rejection, got {cycle_err:?}"
    );
    assert_scratch_empty(&scratch);

    // -----------------------------------------------------------------------
    // 4. Real confined worker fixtures (single-process, no fork).
    // -----------------------------------------------------------------------
    let fixture_dir = tempfile::tempdir().unwrap();
    let secret_file = fixture_dir.path().join("service-secret.txt");
    std::fs::write(&secret_file, b"TOP_SECRET_CREDENTIAL_DO_NOT_LEAK").unwrap();

    let make_worker = |dir: &Path, shebang: &str, body: &str| -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let worker = bin.join("worker");
        std::fs::write(&worker, format!("{shebang}\n{body}\n")).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        worker
    };
    let run_worker = |shebang: &str, body: &str, caps: CheckLibraryCaps| -> Refusal {
        let fdir = tempfile::tempdir().unwrap();
        let fscratch = fdir.path().join("scratch");
        let svc = Service::new(
            make_worker(fdir.path(), shebang, body),
            fscratch.clone(),
            Caps::HOST,
            1,
            0,
        );
        let result = svc
            .check_library_with_caps(&req(leaf_root, &leaf_bytes, &[], &[]), caps)
            .unwrap_err();
        assert_scratch_empty(&fscratch);
        result
    };
    let run_sh = |body: &str, caps: CheckLibraryCaps| run_worker("#!/bin/sh", body, caps);
    let run_py =
        |body: &str, caps: CheckLibraryCaps| run_worker("#!/usr/bin/python3 -S", body, caps);

    let base_caps = CheckLibraryCaps {
        cpu_seconds: 5,
        memory_bytes: 128 << 20,
        wall_ms: 10_000,
        output_bytes: 4096,
        ..CheckLibraryCaps::REGISTRY
    };

    // 4a. Host secret file read must be denied by OS confinement (no fork).
    let read_secret_py = format!(
        "\
import errno, sys
try:
    open({:?}, 'rb').read(1)
    sys.stderr.write('LEAKED_SECRET\\n'); sys.exit(1)
except OSError as e:
    if e.errno not in (errno.EACCES, errno.EPERM):
        sys.stderr.write(f'UNEXPECTED_SECRET_ERRNO={{e.errno}}\\n'); sys.exit(1)
sys.stderr.write('confinement-denied-secret\\n'); sys.exit(1)",
        secret_file.display().to_string()
    );
    assert_eq!(
        run_py(&read_secret_py, base_caps),
        Refusal::Rejected("confinement-denied-secret\n".into())
    );

    // 4b. Inherited host env vars must not leak; HOME/XDG must stay in private scratch.
    let env_py = "\
import os, sys
allowed = {'HIRSEL_MEMORY_LIMIT','HOME','TMPDIR','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_RUNTIME_DIR'}
extra = sorted(k for k in os.environ if k not in allowed and not (k == 'LC_CTYPE' and os.environ[k] == 'C.UTF-8'))
if extra:
    sys.stderr.write(f'ENV_LEAK:{extra}\\n'); sys.exit(1)
if '/check-' not in os.environ.get('HOME', ''):
    sys.stderr.write(f'BAD_HOME={os.environ.get(\"HOME\")}\\n'); sys.exit(1)
sys.stderr.write('env-clean\\n'); sys.exit(1)";
    assert_eq!(
        run_py(env_py, base_caps),
        Refusal::Rejected("env-clean\n".into())
    );

    // 4c. Extra inherited non-CLOEXEC file descriptor must be closed before worker runs.
    let raw_secret_fd = {
        use std::os::fd::IntoRawFd;
        let file = std::fs::File::open(&secret_file).unwrap();
        let raw = file.into_raw_fd();
        // SAFETY: raw is a live descriptor opened above.
        unsafe { libc::fcntl(raw, libc::F_SETFD, 0) };
        raw
    };
    let fd_py = format!(
        "\
import errno, os, sys
try:
    os.read({raw_secret_fd}, 1)
    sys.stderr.write('FD_LEAK\\n'); sys.exit(1)
except OSError as e:
    if e.errno != errno.EBADF:
        sys.stderr.write(f'UNEXPECTED_FD_ERRNO={{e.errno}}\\n'); sys.exit(1)
sys.stderr.write('fd-closed\\n'); sys.exit(1)"
    );
    let fd_res = run_py(&fd_py, base_caps);
    // SAFETY: close the parent test descriptor now.
    unsafe { libc::close(raw_secret_fd) };
    assert_eq!(fd_res, Refusal::Rejected("fd-closed\n".into()));

    // 4d. OS confinement denies both network/Unix sockets and process creation (fork).
    let isolate_py = "\
import errno, os, socket, sys
for family in (socket.AF_INET, socket.AF_UNIX):
    try:
        s = socket.socket(family, socket.SOCK_STREAM)
        s.close()
        sys.stderr.write(f'SOCKET_ALLOWED_{family}\\n'); sys.exit(1)
    except OSError as e:
        if e.errno not in (errno.EPERM, errno.EACCES):
            sys.stderr.write(f'UNEXPECTED_SOCK_ERRNO_{e.errno}\\n'); sys.exit(1)
try:
    pid = os.fork()
    if pid == 0: os._exit(0)
    os.waitpid(pid, 0)
    sys.stderr.write('FORK_ALLOWED\\n'); sys.exit(1)
except OSError as e:
    if e.errno not in (errno.EPERM, errno.EAGAIN):
        sys.stderr.write(f'UNEXPECTED_FORK_ERRNO_{e.errno}\\n'); sys.exit(1)
sys.stderr.write('sockets-and-fork-denied\\n'); sys.exit(1)";
    assert_eq!(
        run_py(isolate_py, base_caps),
        Refusal::Rejected("sockets-and-fork-denied\n".into())
    );

    // 4e. Enforced CPU, wall, real OS memory allocation, output, diagnostics, and unsupported caps.
    assert_eq!(
        run_sh(
            "while :; do :; done",
            CheckLibraryCaps {
                cpu_seconds: 1,
                ..base_caps
            }
        ),
        Refusal::Limit(Limit::Cpu)
    );
    let wall_start = Instant::now();
    assert_eq!(
        run_sh(
            "exec /bin/sleep 60",
            CheckLibraryCaps {
                wall_ms: 250,
                ..base_caps
            }
        ),
        Refusal::Limit(Limit::Wall)
    );
    assert!(wall_start.elapsed() < Duration::from_secs(10));

    // Real memory-cap allocation: ignores HIRSEL_MEMORY_LIMIT and allocates + faults
    // 256 MiB via MAP_SHARED|MAP_ANONYMOUS (which bypasses RLIMIT_DATA on Linux).
    // The delegated cgroup charges the faulted pages to the worker.
    let mem_caps = CheckLibraryCaps {
        memory_bytes: 64 << 20,
        ..base_caps
    };
    let mem_hog_py = "\
import errno, mmap, sys
chunks, chunk = [], 4 << 20
for _ in range(64):
    try:
        m = mmap.mmap(-1, chunk, flags=mmap.MAP_SHARED | mmap.MAP_ANONYMOUS)
    except OSError as e:
        if e.errno == errno.ENOMEM:
            sys.exit(71)
        sys.stderr.write(f'UNEXPECTED_MMAP_ERRNO={e.errno}\\n'); sys.exit(1)
    for off in range(0, chunk, 4096):
        m[off] = 1
    chunks.append(m)
sys.stderr.write('MEMORY_CAP_BYPASSED\\n'); sys.exit(1)";
    assert_eq!(run_py(mem_hog_py, mem_caps), Refusal::Limit(Limit::Memory));
    {
        struct RestoreCgroup;
        impl Drop for RestoreCgroup {
            fn drop(&mut self) {
                host::disable_cgroup_for_test(false);
            }
        }
        host::disable_cgroup_for_test(true);
        let _restore = RestoreCgroup;
        let result = real_service.check_library(&req(
            head_root,
            &head_bytes,
            &valid_closure,
            &valid_history,
        ));
        assert!(
            matches!(result, Err(Refusal::Unavailable(ref why)) if why.contains("cgroup v2")),
            "{result:?}"
        );
        assert_scratch_empty(&scratch);
    }

    assert_eq!(
        run_sh("exec /usr/bin/printf '%4097s' ''", base_caps),
        Refusal::Limit(Limit::Output)
    );
    let noisy = run_py(
        "import sys\nsys.stderr.write('x' * 131072); sys.exit(1)",
        base_caps,
    );
    match noisy {
        Refusal::Rejected(msg) => {
            assert_eq!(msg.len(), CheckLibraryCaps::REGISTRY.diagnostics_bytes)
        }
        other => panic!("expected truncated Refusal::Rejected, got {other:?}"),
    }
    assert_eq!(
        run_sh(
            "echo 'check_library: unsupported: missing toolchain pkg' >&2; exit 2",
            base_caps
        ),
        Refusal::Unsupported("missing toolchain pkg".into())
    );

    // Queue busy law on check_library.
    let busy_dir = tempfile::tempdir().unwrap();
    let busy_scratch = busy_dir.path().join("scratch");
    let busy_worker = make_worker(
        busy_dir.path(),
        "#!/usr/bin/python3 -S",
        "import sys, time\ntime.sleep(0.5)\nsys.exit(1)",
    );
    let busy_service = Service::new(busy_worker, busy_scratch.clone(), Caps::HOST, 1, 0);
    let leaf_req = req(leaf_root, &leaf_bytes, &[], &[]);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| busy_service.check_library_with_caps(&leaf_req, base_caps));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            busy_service.check_library_with_caps(&leaf_req, base_caps),
            Err(Refusal::Busy)
        );
        assert_eq!(first.join().unwrap(), Err(Refusal::Rejected(String::new())));
    });
    assert_scratch_empty(&busy_scratch);
    let checked_64 = exact_64_result.expect("64 MiB raw C must reach decoding and check");
    assert_eq!(checked_64.root, exact_64_root.to_string());
    assert_eq!(checked_64.header.identity, "github.com/acme/exact64@1");
    assert_eq!(checked_64.header.version, "1.0.0");
    assert_eq!(checked_64.header.charge, "charge-v1");
}

mod worker_result_tests;
