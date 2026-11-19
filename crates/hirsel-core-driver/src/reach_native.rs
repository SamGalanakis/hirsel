// The reach record (hirsel.core-reach.v6). Law: a build in a primed store is
// the build of the same sources and options into an empty store: the same
// executable bytes and exit status. On a no-change request it walks nothing
// (no instance on the M2 path, no object checked, no link); on a one-body edit
// only the edited instance takes the M2 path. Oracle for every case: a fresh
// store's build, and the hand-derived exit and compiled count of each step.

/// One build of a step: the product, the executable's bytes and its status.
fn reach_build(product: &CheckProduct, options: &NativeOptions) -> (BuildProduct, Vec<u8>, Option<i32>) {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, options).unwrap();
    let status = Command::new(&built.executable).status().unwrap().code();
    let bytes = std::fs::read(&built.executable).unwrap();
    (built, bytes, status)
}
/// A step: the app source, its exit, and its work in the primed store:
/// instances compiled and instances on the M2 path (`None`: not asserted).
struct Step<'a> {
    app: &'a str,
    exit: i32,
    work: Option<(usize, u64)>,
}
/// Build each step in one store against `core`, each against an empty
/// store, and each step again; the work counters of each repeat are a
/// no-change request's.
fn reach_steps(name: &str, backend: Backend, core: &Path, steps: &[Step]) {
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(backend, dir.path());
    for (n, step) in steps.iter().enumerate() {
        let product = record_check(step.app, core, dir.path(), "store");
        let (built, bytes, status) = reach_build(&product, &options);
        let fresh_store = dir.path().join(format!("fresh-native{n}"));
        let fresh = reach_build(
            &record_check(step.app, core, dir.path(), &format!("fresh{n}")),
            &NativeOptions {
                store: fresh_store.clone(),
                output: dir.path().join(format!("fresh-program{n}")),
                ..native_options(backend, dir.path())
            },
        );
        assert_eq!(status, Some(step.exit), "{name} step {n}: exit");
        assert!(bytes == fresh.1, "{name} step {n}: the primed store's executable is a fresh store's");
        // A record written from a prior one (patched) is the fresh record.
        assert!(
            same_records(&options.store, &fresh_store),
            "{name} step {n}: the primed store's reach record is a fresh store's"
        );
        if let Some((compiled, path)) = step.work {
            assert_eq!(built.compiled, compiled, "{name} step {n}: compiled");
            if matches!(backend, Backend::Cranelift) {
                assert_eq!(built.reach.instance_path, path, "{name} step {n}: M2 path");
            }
        }
        let (again, again_bytes, _) = reach_build(&product, &options);
        assert!(again_bytes == bytes, "{name} step {n}: repeat");
        assert_eq!((again.compiled, again.linked), (0, false), "{name} step {n}: a repeat is a hit");
        if matches!(backend, Backend::Cranelift) {
            assert_eq!(
                (again.reach.instance_path, again.reach.objects_verified),
                (0, 0),
                "{name} step {n}: a no-change request walks and checks nothing"
            );
        }
        // The first build without a record, over the stored objects.
        for (path, _) in reach_records(&options.store) {
            std::fs::remove_file(path).unwrap();
        }
        let (_, unrecorded, _) = reach_build(&product, &options);
        assert!(unrecorded == bytes, "{name} step {n}: a build without a record");
        assert!(
            same_records(&options.store, &fresh_store),
            "{name} step {n}: a record written without a prior is a fresh store's"
        );
    }
}
macro_rules! step {
    ($app:expr, $exit:expr) => {
        Step { app: $app, exit: $exit, work: None }
    };
    ($app:expr, $exit:expr, $compiled:expr, $path:expr) => {
        Step { app: $app, exit: $exit, work: Some(($compiled, $path)) }
    };
}
/// The edit kinds, each a sequence over one store (cases R1-R3, R7, R10,
/// R11, R13, R14 and the shadowing name).
fn reach_edit_kinds(backend: Backend) {
    let core = core_sources();
    const DEEP: &str = "fn c() -> u8 { 7u8 }\nfn b() -> u8 { c() }\nfn a() -> u8 { b() }\npub fn main() -> Result[(), Exit] { Error(Exit(status: a())) }\n";
    // R1: a deep leaf edit compiles the leaf alone.
    reach_steps("R1 deep leaf", backend, &core, &[
        step!(DEEP, 7),
        step!(&DEEP.replace("{ 7u8 }", "{ 9u8 }"), 9, 1, 1),
    ]);
    // R2: a call added, then removed; the removed callee leaves the link.
    const ONE: &str = "fn f() -> u8 { 3u8 }\nfn g() -> u8 { 4u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: f())) }\n";
    reach_steps("R2 calls", backend, &core, &[
        step!(ONE, 3),
        step!(&ONE.replace("status: f()", "status: f() + g()"), 7, 2, 2),
        step!(ONE, 3, 0, 1),
    ]);
    // R3: an unused constant is checked (LANG-20), so it enters the record.
    const CONSTANT: &str = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 5u8)) }\n";
    reach_steps("R3 constant", backend, &core, &[
        step!(CONSTANT, 5),
        step!(&format!("{CONSTANT}const UNUSED: u8 = 9u8\n"), 5),
        step!(CONSTANT, 5),
    ]);
    // R7: a header-only change: an unreached function's signature.
    const HEADER: &str = "fn spare(x: u8) -> u8 { x }\npub fn main() -> Result[(), Exit] { Error(Exit(status: 6u8)) }\n";
    reach_steps("R7 header", backend, &core, &[
        step!(HEADER, 6),
        step!(&HEADER.replace("fn spare(x: u8) -> u8 { x }", "fn spare(x: u16) -> u16 { x }"), 6, 0, 0),
    ]);
    // R10: two programs alternate in one store under one entry path. The
    // record is the entry's, so each takes the other's: every row differs,
    // each instance walks its record (DEEP: main, a, b, c; OTHER: main, k),
    // and none compiles again.
    const OTHER: &str = "fn k() -> u8 { 2u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: k())) }\n";
    reach_steps("R10 two programs", backend, &core, &[
        step!(DEEP, 7),
        step!(OTHER, 2),
        step!(DEEP, 7, 0, 4),
        step!(OTHER, 2, 0, 2),
    ]);
    // R11: glue (a generic instance) reached through a changed parent.
    const GLUE: &str = "fn id[T](take x: T) -> T { x }\nfn p() -> u8 { id(4u8) }\npub fn main() -> Result[(), Exit] { Error(Exit(status: p())) }\n";
    reach_steps("R11 glue", backend, &core, &[
        step!(GLUE, 4),
        step!(&GLUE.replace("id(4u8)", "id(5u8)"), 5, 1, 1),
        step!(&GLUE.replace("id(4u8)", "id(id(5u8))"), 5, 1, 1),
    ]);
    // R13: SourceMap-only movement: trailing lines move no row, one relink.
    reach_steps("R13 trailing lines", backend, &core, &[
        step!(DEEP, 7),
        step!(&format!("{DEEP}\n\n\n"), 7, 0, 0),
    ]);
    // R14: two calls swapped in the entry's body: the link order is fresh's.
    const ORDER: &str = "fn x() -> u8 { 1u8 }\nfn y() -> u8 { 2u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: x() + y())) }\n";
    reach_steps("R14 order", backend, &core, &[
        step!(ORDER, 3),
        step!(&ORDER.replace("x() + y()", "y() + x()"), 3, 1, 1),
    ]);
}
#[test]
#[ignore = "explicit native integration target"]
fn reach_edit_kinds_cranelift() {
    reach_edit_kinds(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn reach_edit_kinds_llvm() {
    reach_edit_kinds(Backend::Llvm);
}

/// A core whose prelude adds `exit_with`, returning `status + bump`.
fn reach_core(dir: &Path, name: &str, bump: u8) -> PathBuf {
    let core = dir.join(name);
    copy_tree(&core_sources(), &core);
    let prelude = std::fs::read_to_string(core.join("core.hirsel")).unwrap();
    std::fs::write(
        core.join("core.hirsel"),
        format!("{prelude}\npub fn exit_with(status: u8) -> Exit {{ Exit(status: status + {bump}u8) }}\n"),
    )
    .unwrap();
    core
}
/// Two cores: one app built against two core sources in one store,
/// alternately; and a name the app declares that shadows the core's.
#[test]
#[ignore = "explicit native integration target"]
fn reach_two_images_and_a_shadowing_name() {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(exit_with(7u8)) }\n";
    let dir = tempfile::tempdir().unwrap();
    let [a, b] = [("core-a", 0), ("core-b", 1)].map(|(name, bump)| reach_core(dir.path(), name, bump));
    let options = native_options(Backend::Cranelift, dir.path());
    for (n, (core, exit)) in [(&a, 7), (&b, 8), (&a, 7), (&b, 8)].into_iter().enumerate() {
        let (built, bytes, status) = reach_build(&record_check(APP, core, dir.path(), "store"), &options);
        let fresh = reach_build(
            &record_check(APP, core, dir.path(), &format!("fresh{n}")),
            &NativeOptions {
                store: dir.path().join(format!("fresh-native{n}")),
                output: dir.path().join(format!("fresh-program{n}")),
                ..native_options(Backend::Cranelift, dir.path())
            },
        );
        assert_eq!(status, Some(exit), "core {n}");
        assert!(bytes == fresh.1, "core {n}: a fresh store's executable");
        if n >= 1 {
            // The cores differ in one body: its instance alone walks, a hit
            // after the first time each core is built.
            let compiled = usize::from(n == 1);
            assert_eq!((built.compiled, built.reach.instance_path), (compiled, 1), "core {n}");
        }
    }
    let shadow = format!("{APP}fn exit_with(status: u8) -> Exit {{ Exit(status: status + 2u8) }}\n");
    reach_steps("shadowing name", Backend::Cranelift, &a, &[
        step!(APP, 7),
        step!(&shadow, 9),
        step!(APP, 7),
    ]);
}

/// The two cores under the release build, where a part's object is keyed by
/// its post-O3 bitcode and core parts live in the shared cache: a core body
/// edit with the same signature must reach the executable (the exits are
/// hand-derived: 7 + bump), and an unchanged rebuild whose unit record is
/// gone generates no part and links the same bytes.
#[test]
#[ignore = "explicit native integration target"]
fn reach_two_cores_release() {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(exit_with(7u8)) }\n";
    let dir = tempfile::tempdir().unwrap();
    let [a, b] = [("core-a", 0), ("core-b", 1)].map(|(name, bump)| reach_core(dir.path(), name, bump));
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    let store = Store::new(&options.store).unwrap();
    for (n, (core, exit)) in [(&a, 7), (&b, 8), (&a, 7), (&b, 8)].into_iter().enumerate() {
        let product = record_check(APP, core, dir.path(), "store");
        let (built, bytes, status) = reach_build(&product, &options);
        assert_eq!(status, Some(exit), "core {n}");
        if n >= 2 {
            assert_eq!(built.compiled, 0, "core {n}: built before");
        }
        let mut units = 0;
        for entry in std::fs::read_dir(&options.store).unwrap() {
            let path = entry.unwrap().path();
            let Some(key) = path.file_name().and_then(|n| n.to_str()).and_then(|n| {
                let bytes = (0..32).map(|i| u8::from_str_radix(n.get(i * 2..i * 2 + 2)?, 16).ok()).collect::<Option<Vec<u8>>>()?;
                Some(ContentHash(bytes.try_into().ok()?))
            }) else { continue };
            if store.get(key).unwrap().is_some_and(|p| super::release::is_unit_record(&p)) {
                std::fs::remove_file(path).unwrap();
                units += 1;
            }
        }
        assert!(units >= 1, "core {n}: a unit record");
        let (again, again_bytes, _) = reach_build(&product, &options);
        assert_eq!(again.compiled, 0, "core {n}: every part is reused");
        assert!(again_bytes == bytes, "core {n}: the same executable");
    }
}

/// A recursive shared type under the release build: its release function's
/// descriptor constant points back at the function, so the function's part
/// holds a constant that names the part's own definition (922-graph crashed
/// writing that part). Exit hand-derived: 1 + 2 + 3 + 4 = 10.
#[test]
#[ignore = "explicit native integration target"]
fn release_part_constant_names_its_own_function() {
    const APP: &str = "type N: shareable + send {\n    value: u64,\n    tail: Option[Arc[N]]\n}\nfn sum(node: Arc[N]) -> u64 {\n    node.value + match node.tail {\n        Some(t) => sum(t),\n        None => 0u64\n    }\n}\npub fn main() -> Result[(), Exit] {\n    let mut root: Option[Arc[N]] = None\n    for i in 1u64 .. 5u64 {\n        let old = replace(mut root, None)\n        root = match arc.new(N(value: i, tail: take old)) {\n            Ok(a) => Some(a),\n            Error(_) => None\n        }\n    }\n    let total = match root {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    Error(Exit(status: int.truncate(total)))\n}\n";
    let dir = tempfile::tempdir().unwrap();
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    let (_, _, status) = reach_build(&record_check(APP, &core_sources(), dir.path(), "store"), &options);
    assert_eq!(status, Some(10));
}

/// A release part is a function of its own key: its object is the same
/// bytes whether it was generated alone or beside the program's other
/// misses. The second build of `P` (same parts, a different `main`) reuses
/// every part but `main`'s and generates `main` alone; a build of the same
/// source into empty parts and store generates `main` beside every other
/// part. Each object both parts caches hold under one name is the same
/// bytes, with and without debug lines. Bug caught: code generation that
/// uses another definition in the same module (interprocedural register
/// allocation) or numbers shared constants per module makes `main`'s object
/// depend on what else missed; core's `int.parse` part makes no call, and
/// `main` holds `acc` across it, so knowing its registers changes `main`'s.
#[test]
#[ignore = "explicit native integration target"]
fn release_part_alone_is_the_part_beside_other_misses() {
    const P: &str = "type N: shareable + send {\n    value: u64,\n    tail: Option[Arc[N]]\n}\nfn sum(node: Arc[N]) -> u64 {\n    node.value + match node.tail {\n        Some(t) => sum(t),\n        None => 0u64\n    }\n}\nfn build(last: u64) -> Option[Arc[N]] {\n    let mut root: Option[Arc[N]] = None\n    for i in 1u64 .. last {\n        let old = replace(mut root, None)\n        root = match arc.new(N(value: i, tail: take old)) {\n            Ok(a) => Some(a),\n            Error(_) => None\n        }\n    }\n    root\n}\npub fn main() -> Result[(), Exit] {\n    let a = match build(LAST) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let b = match build(3u64) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let mut s = string.new()\n    let _ = string.push_u64(mut s, a)\n    let _ = string.push_char(mut s, ' ')\n    let _ = string.push_u64(mut s, b)\n    let n = string.byte_len(s)\n    let mut acc = a + b\n    for i in 0u64 .. n {\n        let parsed: Result[i64, ParseError] = int.parse(string.slice(s, i, n))\n        acc = acc + match parsed {\n            Ok(v) => int.truncate(v),\n            Error(_) => 1u64\n        }\n    }\n    Error(Exit(status: int.truncate(acc)))\n}\n";
    let objects = |root: &Path| -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
        std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter_map(|p| {
                let bytes = std::fs::read(&p).ok()?;
                bytes.starts_with(b"\x7fELF").then(|| (p.file_name().unwrap().to_owned(), bytes))
            })
            .collect()
    };
    for debug_lines in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let [first, second] = ["6u64", "7u64"].map(|last| P.replace("LAST", last));
        let mut options = native_options(Backend::Llvm, dir.path());
        options.optimize = true;
        options.debug_lines = debug_lines;
        // Exits: a = 1 + .. + (LAST - 1), b = 1 + 2, then the suffixes of "{a} {b}" parsed as i64:
        // "15 3", "5 3" and " 3" do not parse (1 each), "3" is 3; so 15 + 3 + 6 = 24, and
        // 21 + 3 + 6 = 30.
        let (cold, _, status) = reach_build(&record_check(&first, &core_sources(), dir.path(), "store"), &options);
        assert_eq!(status, Some(24), "debug lines {debug_lines}");
        let (mixed, bytes, status) = reach_build(&record_check(&second, &core_sources(), dir.path(), "store"), &options);
        assert_eq!(status, Some(30), "debug lines {debug_lines}");
        assert!(
            mixed.compiled >= 1 && mixed.compiled < cold.compiled,
            "debug lines {debug_lines}: a mixed build ({} of {} parts)",
            mixed.compiled,
            cold.compiled
        );
        let fresh_parts = dir.path().join("fresh-parts");
        let (fresh_build, fresh_bytes, _) = reach_build(
            &record_check(&second, &core_sources(), dir.path(), "fresh"),
            &NativeOptions {
                store: dir.path().join("fresh-native"),
                parts: fresh_parts.clone(),
                output: dir.path().join("fresh-program"),
                optimize: true,
                debug_lines,
                ..native_options(Backend::Llvm, dir.path())
            },
        );
        assert_eq!(fresh_build.compiled, cold.compiled, "debug lines {debug_lines}: a fresh build generates every part");
        let (primed, fresh) = (objects(&options.parts), objects(&fresh_parts));
        let shared = fresh.keys().filter(|name| primed.contains_key(*name)).count();
        assert!(shared >= 2, "debug lines {debug_lines}: parts in both stores");
        for (name, object) in &fresh {
            if let Some(alone) = primed.get(name) {
                assert!(alone == object, "debug lines {debug_lines}: part {name:?} alone and beside the other misses");
            }
        }
        assert!(bytes == fresh_bytes, "debug lines {debug_lines}: the executable");
    }
}

/// The release build's edits in one store and parts cache are a cold
/// build of the same source: the same executable bytes. Each kind reuses
/// unchanged functions' cached post-O3 tails beside the edited ones: a body
/// edit, an edit to a helper its caller inlines, a signature change, and a
/// core (std-visible) body edit. With line tables on, the body edit also
/// shifts the unchanged recursive function and its callers down two lines.
/// Mutants caught: dropping the key's bitcode reuses a stale tail, and
/// skipping scope rebasing on restore loses debug rows. Rebuilding each
/// debug tail in a fresh context from its key also checks that the tail can
/// run without the rest of the module. Exits hand-derived
/// as in `release_part_alone_is_the_part_beside_other_misses`, with each
/// value scaled: a = scale(1 + .. + (LAST - 1)), b = scale(1 + 2), plus
/// the parsed suffixes of "{a} {b}" (1 each that does not parse), plus the
/// core's bump: 15 + 3 + 6 = 24; 21 + 3 + 6 = 30; x2: 42 + 6 + 9 = 57;
/// x3: 63 + 9 + 12 = 84; bump 1: 85.
#[test]
#[ignore = "explicit native integration target"]
fn release_edit_is_the_cold_build() {
    const P: &str = "type N: shareable + send {\n    value: u64,\n    tail: Option[Arc[N]]\n}\nfn scale(x: u64) -> u64 { x }\nfn sum(node: Arc[N]) -> u64 {\n    scale(node.value) + match node.tail {\n        Some(t) => sum(t),\n        None => 0u64\n    }\n}\nfn build(last: u64) -> Option[Arc[N]] {\n    let mut root: Option[Arc[N]] = None\n    for i in 1u64 .. last {\n        let old = replace(mut root, None)\n        root = match arc.new(N(value: i, tail: take old)) {\n            Ok(a) => Some(a),\n            Error(_) => None\n        }\n    }\n    root\n}\npub fn main() -> Result[(), Exit] {\n    let a = match build(6u64) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let b = match build(3u64) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let mut s = string.new()\n    let _ = string.push_u64(mut s, a)\n    let _ = string.push_char(mut s, ' ')\n    let _ = string.push_u64(mut s, b)\n    let n = string.byte_len(s)\n    let mut acc = a + b\n    for i in 0u64 .. n {\n        let parsed: Result[i64, ParseError] = int.parse(string.slice(s, i, n))\n        acc = acc + match parsed {\n            Ok(v) => int.truncate(v),\n            Error(_) => 1u64\n        }\n    }\n    Error(exit_with(int.truncate(acc)))\n}\n";
    for debug_lines in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let [a, b] = [("core-a", 0), ("core-b", 1)].map(|(name, bump)| reach_core(dir.path(), name, bump));
        let body = P.replace("build(6u64)", "build(7u64)");
        let body = if debug_lines {
            body.replace("fn sum(", "\n\nfn sum(")
        } else {
            body
        };
        let inlined = body.replace("{ x }", "{ x * 2u64 }");
        let signature = inlined
            .replace("fn scale(x: u64) -> u64 { x * 2u64 }", "fn scale(x: u64, m: u64) -> u64 { x * m }")
            .replace("scale(node.value)", "scale(node.value, 3u64)");
        let steps = [
            ("cold", P, &a, 24),
            ("body", &body, &a, 30),
            ("inlined helper", &inlined, &a, 57),
            ("signature", &signature, &a, 84),
            ("std-visible", &signature, &b, 85),
        ];
        let mut options = native_options(Backend::Llvm, dir.path());
        options.optimize = true;
        options.debug_lines = debug_lines;
        for (n, (kind, app, core, exit)) in steps.into_iter().enumerate() {
            let check_tails = debug_lines && n == 0;
            if check_tails {
                release::TAIL_FIXTURES.with(|fixtures| *fixtures.borrow_mut() = Some(Vec::new()));
            }
            let (built, bytes, status) = reach_build(&record_check(app, core, dir.path(), "store"), &options);
            if check_tails {
                let fixtures = release::TAIL_FIXTURES.with(|fixtures| fixtures.take().unwrap());
                assert!(!fixtures.is_empty(), "debug release writes tail records");
                let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                let tool = root.join(std::env::var_os("HIRSEL_TEST_TAIL_CONTEXT").unwrap());
                let llvm = hirsel_llvm_lazy::llvm_library().unwrap();
                let mut built_keys = 0;
                for fixture in fixtures {
                    let paths = [
                        ("tail-key", fixture.key),
                        ("tail-input", fixture.input),
                        ("tail-cached", fixture.cached),
                        ("tail-ir", fixture.ir.to_vec()),
                    ]
                        .map(|(name, data)| {
                            let path = dir.path().join(name);
                            std::fs::write(&path, data).unwrap();
                            path
                        });
                    let run = Command::new(&tool).args(paths)
                        .env("LD_LIBRARY_PATH", llvm.parent().unwrap())
                        .stderr(std::process::Stdio::inherit())
                        .output().unwrap();
                    assert!(
                        run.status.success(),
                        "the part's key is exact and the tail rebuilt using only the key equals the cached tail"
                    );
                    built_keys += usize::from(run.stdout == b"built key\n");
                }
                assert!(built_keys > 0, "some part read back has the key the build computed");
            }
            let (cold, cold_bytes, _) = reach_build(
                &record_check(app, core, dir.path(), &format!("fresh{n}")),
                &NativeOptions {
                    store: dir.path().join(format!("fresh-native{n}")),
                    parts: dir.path().join(format!("fresh-parts{n}")),
                    output: dir.path().join(format!("fresh-program{n}")),
                    optimize: true,
                    debug_lines,
                    ..native_options(Backend::Llvm, dir.path())
                },
            );
            assert_eq!(status, Some(exit), "{kind} (debug lines {debug_lines}): exit");
            if n > 0 {
                assert!(built.compiled >= 1 && built.compiled < cold.compiled, "{kind} (debug lines {debug_lines}): {} of {} parts", built.compiled, cold.compiled);
            }
            assert!(bytes == cold_bytes, "{kind} (debug lines {debug_lines}): the edited build is the cold build");
        }
    }
}

/// The reach records a native store holds: files named by an address whose
/// checked record reads as one, with their payloads.
fn reach_records(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let store = Store::new(root).unwrap();
    let mut records: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let bytes = (0..32)
                .map(|i| u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            let record = store.get(ContentHash(bytes.try_into().ok()?)).ok()??;
            crate::reach::Reach::read(record.clone()).map(|_| (path, record))
        })
        .collect();
    records.sort();
    records
}
/// Each reach record of the `fresh` store is in `primed` under its name,
/// byte for byte.
fn same_records(primed: &Path, fresh: &Path) -> bool {
    let primed = reach_records(primed);
    reach_records(fresh).iter().all(|(path, bytes)| {
        primed
            .iter()
            .any(|(p, b)| p.file_name() == path.file_name() && b == bytes)
    })
}
const REACH_APP: &str = "fn get(x: u8) -> u8 { x + 1u8 }\nfn base() -> u8 { 5u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(base()))) }\n";

/// R8: a damaged reach record is a miss: the build walks again, equals a
/// fresh store's, and writes a record the next request uses.
#[test]
#[ignore = "explicit native integration target"]
fn reach_damaged_record_walks_again() {
    let core = core_sources();
    for how in [Damage::Middle, Damage::Truncate] {
        let dir = tempfile::tempdir().unwrap();
        let options = native_options(Backend::Cranelift, dir.path());
        let product = record_check(REACH_APP, &core, dir.path(), "store");
        let (_, first, _) = reach_build(&product, &options);
        let records = reach_records(&options.store);
        assert_eq!(records.len(), 1, "{how:?}: one reach record");
        damage(&records[0].0, how);
        let (built, bytes, status) = reach_build(&product, &options);
        assert_eq!(status, Some(6), "{how:?}");
        assert!(bytes == first, "{how:?}: the first build's executable");
        assert!(built.reach.instance_path > 0, "{how:?}: the damaged record is not used");
        let (again, _, _) = reach_build(&product, &options);
        assert_eq!(again.reach.instance_path, 0, "{how:?}: the new record is used");
    }
}

/// R12 (replace): every stored object is replaced by another valid object,
/// then one body is edited: each replaced object fails its check, is
/// rebuilt, and the executable is a fresh store's. Of the n stored objects
/// the n - 1 not of `base` are linked: the first link checks and refuses
/// each, and one retry rebuilds them all, so the request compiles n (the
/// edited `base` and each refused object once) and checks n objects (the
/// n - 1, and `base`'s, which the retry reuses from its record). Bug caught (red at d0d1ee55): the link stopped at the first
/// refusal and each retry rebuilt every instance refused so far, compiling
/// 1 + 2 + ... and dropping the earlier attempts' counts.
#[test]
#[ignore = "explicit native integration target"]
fn reach_replaced_objects_build_a_fresh_stores_executable() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    reach_build(&record_check(REACH_APP, &core, dir.path(), "store"), &options);
    let objects = stored_objects(&options.store);
    assert!(objects.len() >= 2);
    let bytes: Vec<_> = objects.iter().map(|p| std::fs::read(p).unwrap()).collect();
    for (i, object) in objects.iter().enumerate() {
        std::fs::write(object, &bytes[(i + 1) % bytes.len()]).unwrap();
    }
    let edited = REACH_APP.replace("{ 5u8 }", "{ 6u8 }");
    let (built, rebuilt, status) = reach_build(&record_check(&edited, &core, dir.path(), "store"), &options);
    let fresh = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert_eq!(status, Some(7));
    assert!(rebuilt == fresh.1, "a fresh store's executable");
    let n = objects.len();
    assert_eq!(
        (built.compiled, built.reach.objects_refused, built.reach.objects_verified),
        (n, n as u64 - 1, n as u64),
        "compiled, refused and checked objects"
    );
}

/// R12b: after the link checks the objects, another valid object is renamed
/// over each checked path; the linker reads the checked inodes, so the
/// executable is a fresh store's.
#[test]
#[ignore = "explicit native integration target"]
fn reach_inode_swap_after_the_check_links_the_checked_object() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    reach_build(&record_check(REACH_APP, &core, dir.path(), "store"), &options);
    let swap = dir.path().join("swap");
    AFTER_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |paths: &[PathBuf]| {
            let bytes: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
            for (i, path) in paths.iter().enumerate() {
                std::fs::write(&swap, &bytes[(i + 1) % bytes.len()]).unwrap();
                std::fs::rename(&swap, path).unwrap();
            }
        }));
    });
    let edited = REACH_APP.replace("{ 5u8 }", "{ 6u8 }");
    let (built, swapped, status) = reach_build(&record_check(&edited, &core, dir.path(), "store"), &options);
    AFTER_CHECK.with(|hook| *hook.borrow_mut() = None);
    assert!(built.linked && built.reach.objects_verified > 0, "the link checked stored objects");
    let fresh = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert_eq!(status, Some(7));
    assert!(swapped == fresh.1, "the checked inodes were linked");
}

/// R12c: between emission and the link's first open, a valid object of the
/// same program with `leaf` returning 42 is renamed over the stored object
/// the build will link (a cached one on a development engine, the program's
/// one object on release). On every engine and mode the linker reads the
/// bytes the build checked or wrote, so the executable is a fresh store's,
/// and so is the next request's: no wrong executable was published under the
/// right objects. Bug caught (red at f488cd81): LLVM checked a cached object
/// in `native_object::load` and the link opened its path again unchecked,
/// linking the swapped inode (exit 42) and caching that executable.
#[test]
#[ignore = "explicit native integration target"]
fn reach_object_swapped_before_the_link_opens_it_links_the_checked_bytes() {
    const A: &str = "fn leaf() -> u8 { 41u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: leaf())) }\n";
    let core = core_sources();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let leaf = DefPath::from_full("app@1/main/leaf").unwrap();
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        let how = format!("{backend:?} optimize={optimize}");
        let dir = tempfile::tempdir().unwrap();
        let base = || NativeOptions { optimize, ..native_options(backend, dir.path()) };
        let options = base();
        let at = |name: &str| NativeOptions {
            store: dir.path().join(format!("{name}-native")),
            output: dir.path().join(format!("{name}-program")),
            ..base()
        };
        let target = |objects: Vec<ObjectProduct>| {
            let mut objects = objects.into_iter();
            if optimize {
                objects.next().unwrap()
            } else {
                objects.find(|o| o.instance.definition == leaf).unwrap()
            }
        };
        assert_eq!(reach_build(&record_check(A, &core, dir.path(), "store"), &options).2, Some(41), "{how}: prime");
        // B forces a link; its target object is stored before the build.
        let source = format!("{A}\n");
        let b = record_check(&source, &core, dir.path(), "store");
        let path = target(b.emit_reachable(&entry, &options).unwrap()).path;
        let donor = record_check(&A.replace("41u8", "42u8"), &core, dir.path(), "donor");
        let donor = std::fs::read(target(donor.emit_reachable(&entry, &at("donor")).unwrap()).path).unwrap();
        let swap = dir.path().join("swap");
        let mut once = true;
        BEFORE_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |paths: &[PathBuf]| {
                if once && paths.contains(&path) {
                    once = false;
                    std::fs::write(&swap, &donor).unwrap();
                    std::fs::rename(&swap, &path).unwrap();
                }
            }))
        });
        let (_, swapped, status) = reach_build(&b, &options);
        BEFORE_OPEN.with(|hook| *hook.borrow_mut() = None);
        let fresh = reach_build(&record_check(&source, &core, dir.path(), "fresh"), &at("fresh"));
        assert_eq!(fresh.2, Some(41), "{how}: fresh");
        assert_eq!(status, Some(41), "{how}: the checked bytes were linked");
        assert!(swapped == fresh.1, "{how}: a fresh store's executable");
        let (_, again, status) = reach_build(&b, &options);
        assert_eq!(status, Some(41), "{how}: next request");
        assert!(again == fresh.1, "{how}: the next request's executable is a fresh store's");
    }
}

/// Review 1012: builds on parallel threads of one process, each executed at
/// once, are launched on every engine and mode. A writable descriptor of one
/// thread's executable copied into another thread's fork made the launch fail
/// with ETXTBSY (45 of 128 on Cranelift before the fix). Oracle: PROG-3, exit
/// 19 for `Error(Exit(status: 19))`.
#[test]
#[ignore = "explicit native integration target"]
fn parallel_builds_launch_at_once() {
    const APP: &str = "fn leaf()->u8{19u8}\npub fn main()->Result[(),Exit]{Error(Exit(status:leaf()))}\n";
    let core = core_sources();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        let refused: usize = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|thread| {
                    let (dir, core, entry, barrier) = (dir.path(), &core, &entry, &barrier);
                    scope.spawn(move || {
                        let here = dir.join(format!("thread-{thread}"));
                        std::fs::create_dir_all(&here).unwrap();
                        let product = record_check(APP, core, &here, "check");
                        let mut options = NativeOptions {
                            store: dir.join("shared-native"),
                            optimize,
                            ..native_options(backend, &here)
                        };
                        barrier.wait();
                        let mut refused = 0;
                        for n in 0..16 {
                            options.output = here.join(format!("program-{n}"));
                            let built = product.build(entry, &options).unwrap();
                            match Command::new(&built.executable).status() {
                                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => refused += 1,
                                status => assert_eq!(status.unwrap().code(), Some(19)),
                            }
                        }
                        refused
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).sum()
        });
        assert_eq!(refused, 0, "{backend:?} release={optimize}: launches refused with ETXTBSY");
    }
}

/// Build `files` (name, source) as project `project` over std imaged into
/// `dir/cache`, with debug lines, into `store` on `backend`: the executable's
/// bytes and its run's exit and stderr.
fn reach_files_build(dir: &Path, project: &str, files: &[(&str, &str)], store: &str, backend: Backend) -> (Vec<u8>, (Option<i32>, Vec<u8>)) {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join(project);
    if project.exists() {
        std::fs::remove_dir_all(&project).unwrap();
    }
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    for (name, source) in files {
        std::fs::write(project.join(name), source).unwrap();
    }
    let request = CheckRequest::program(&project, &libraries, &dir.join(store).join("check")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join("cache"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let mut options = native_options(backend, dir);
    options.store = dir.join(store).join("native");
    options.output = dir.join(store).join("program");
    options.debug_lines = true;
    options.grant_mask = 1 << 4;
    let built = product.build(&DefPath::from_full("app@1/main/main").unwrap(), &options).unwrap();
    let output = Command::new(&built.executable).output().unwrap();
    (std::fs::read(&built.executable).unwrap(), (output.status.code(), output.stderr))
}

/// A link's source places (each reached item's file and offset) are the
/// current sources', whichever the record that a primed store holds says:
/// the record's place of an item in an image file is borrowed, the rest found
/// again. Each step is built in one primed store and in an empty one, with
/// debug lines, over a program in three files whose failure report names its
/// line: blank lines above a reached item (its row holds, its place moves), an
/// edit in a file the link reaches nothing in, a file added ahead of the
/// others, an item added above a reached one, and an item moved between
/// files. Oracle: the empty store's executable. Mutant: every recorded place
/// borrowed, red (the blank lines move the reported line).
#[test]
#[ignore = "explicit native integration target"]
fn reach_source_places_are_the_current_sources() {
    const MAIN: &str = "pub fn main() -> Result[(), Exit] {\n    if string.starts_with(\"ab\", \"a\") && other.f(200u8) > 0u8 { Ok(()) } else { Error(Exit(status: 3)) }\n}\n";
    const OTHER: &str = "pub fn f(x: u8) -> u8 {\n    x + 100u8\n}\n";
    const SPARE: &str = "pub fn unused() -> u8 { 1u8 }\n";
    let moved_main = "pub fn main() -> Result[(), Exit] {\n    if string.starts_with(\"ab\", \"a\") && f(200u8) > 0u8 { Ok(()) } else { Error(Exit(status: 3)) }\n}\nfn f(x: u8) -> u8 {\n    x + 100u8\n}\n";
    let shifted = format!("pub fn g() -> u8 {{\n    2u8\n}}\n{OTHER}");
    let blank = format!("\n\n\n{OTHER}");
    let steps: [&[(&str, &str)]; 6] = [
        &[("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", SPARE)],
        &[("main.hirsel", MAIN), ("other.hirsel", &blank), ("spare.hirsel", SPARE)],
        &[("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", "\n\npub fn unused() -> u8 { 2u8 }\n")],
        &[("aaa.hirsel", SPARE), ("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", SPARE)],
        &[("aaa.hirsel", SPARE), ("main.hirsel", MAIN), ("other.hirsel", &shifted), ("spare.hirsel", SPARE)],
        &[("aaa.hirsel", SPARE), ("main.hirsel", moved_main), ("other.hirsel", SPARE), ("spare.hirsel", SPARE)],
    ];
    let lines = ["app@1/other.hirsel:2", "app@1/other.hirsel:5", "app@1/other.hirsel:2", "app@1/other.hirsel:2", "app@1/other.hirsel:5", "app@1/main.hirsel:5"];
    let dir = tempfile::tempdir().unwrap();
    for (n, files) in steps.iter().enumerate() {
        let (primed, ran) = reach_files_build(dir.path(), "app", files, "primed", Backend::Cranelift);
        let (fresh, expected) = reach_files_build(dir.path(), "app", files, &format!("fresh{n}"), Backend::Cranelift);
        assert_eq!(ran.0, Some(101), "step {n}: {:?}", String::from_utf8_lossy(&ran.1));
        assert!(String::from_utf8_lossy(&ran.1).contains(lines[n]), "step {n}: {}", String::from_utf8_lossy(&ran.1));
        assert_eq!(ran, expected, "step {n}: what the program reports");
        assert!(primed == fresh, "step {n}: the primed store's executable is a fresh store's");
    }
}

/// A generic body that reads `code.identity` reads it from the StaticFn's
/// descriptor, so its record holds no program's function list and a store
/// serves it to any program: a build in the primed store is the build of the
/// same files into an empty one. Steps over one store: a program with a spare
/// module; the spare module dropped; a new module whose function the entry
/// describes, so its descriptor is reached only through the entry's
/// `describe`. Oracle: the empty store's executable, and exit 0 (the identity
/// matches). This is the case of the fault where a store served a body that
/// dispatched over another program's function table (reuse-history-fault-1031);
/// its mutant, the table left out of the key, went with the table. The
/// descriptor's own mutant is `reach_code_descriptor_is_one_per_function`'s.
#[test]
#[ignore = "explicit native integration target"]
fn reach_code_identity_follows_the_program() {
    const NAMED: &str = "fn named[F](s: StaticFn[F], want: Bytes) -> bool {\n    code.identity(s) == want\n}\nfn double(x: u64) -> u64 {\n    x * 2\n}\n";
    let local = format!("{NAMED}pub fn main() -> Result[(), Exit] {{\n    if named(code.describe(double), code.identity(code.describe(double))) {{ Ok(()) }} else {{ Error(Exit(status: 3)) }}\n}}\n");
    let other = format!("{NAMED}pub fn main() -> Result[(), Exit] {{\n    if named(code.describe(other.pick), code.identity(code.describe(other.pick))) {{ Ok(()) }} else {{ Error(Exit(status: 3)) }}\n}}\n");
    let steps: [&[(&str, &str)]; 3] = [
        &[("main.hirsel", &local), ("spare.hirsel", "pub fn spare(x: u64) -> u64 { x }\n")],
        &[("main.hirsel", &local)],
        &[("main.hirsel", &other), ("other.hirsel", "pub fn pick(x: u64) -> u64 { x + 1 }\n")],
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let dir = tempfile::tempdir().unwrap();
        for (n, files) in steps.iter().enumerate() {
            let case = format!("{backend:?} step {n}");
            let (primed, ran) = reach_files_build(dir.path(), "app", files, "primed", backend);
            let (fresh, expected) = reach_files_build(dir.path(), "app", files, &format!("fresh{n}"), backend);
            assert_eq!(expected.0, Some(0), "{case}: {}", String::from_utf8_lossy(&expected.1));
            assert_eq!(ran, expected, "{case}: what the program reports");
            assert!(primed == fresh, "{case}: the primed store's executable is a fresh store's");
        }
    }
}

/// A flipped byte in any section of the reach record is a miss: the build
/// walks again, equals the first, and writes the record afresh. A section
/// with no bytes has its head length flipped. Buck target: core-driver-native.
#[test]
#[ignore = "explicit native integration target"]
fn reach_flipped_section_byte_is_a_miss() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    let product = record_check(REACH_APP, &core, dir.path(), "store");
    let (_, first, _) = reach_build(&product, &options);
    let [(path, record)] = &reach_records(&options.store)[..] else {
        panic!("one reach record");
    };
    let sections = crate::reach::Reach::read(record.clone()).unwrap().sections();
    for (i, (offset, len)) in sections.into_iter().enumerate() {
        let at = match len {
            0 => crate::reach::HEAD_SECTIONS + i * 8 + 4,
            _ => offset + len / 2,
        };
        let mut file = std::fs::read(path).unwrap();
        file[crate::store::HEADER + at] ^= 0x10;
        std::fs::write(path, &file).unwrap();
        let (built, bytes, status) = reach_build(&product, &options);
        assert_eq!(status, Some(6), "section {i}");
        assert!(bytes == first, "section {i}: the first build's executable");
        assert!(built.reach.instance_path > 0, "section {i}: the flipped record is not used");
        assert!(reach_records(&options.store) == [(path.clone(), record.clone())], "section {i}: the record afresh");
    }
}

/// A record exists only for an accepted, complete walk whose executable is
/// stored: a walk refused at its instance limit publishes none, and an
/// output that cannot be written after the link leaves the record, which the
/// next build then uses as a hit. Oracle: a fresh store's record bytes.
/// Buck target: core-driver-native.
#[test]
#[ignore = "explicit native integration target"]
fn reach_record_only_for_a_complete_walk_and_kept_past_an_output_failure() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let product = record_check(REACH_APP, &core, dir.path(), "store");
    let options = native_options(Backend::Cranelift, dir.path());
    let refused = NativeOptions { instance_limit: 0, ..native_options(Backend::Cranelift, dir.path()) };
    assert!(product.build(&entry, &refused).is_err(), "the walk is refused at its limit");
    assert!(reach_records(&options.store).is_empty(), "a refused walk publishes no record");
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"").unwrap();
    let unwritable = NativeOptions { output: blocked.join("program"), ..native_options(Backend::Cranelift, dir.path()) };
    assert!(product.build(&entry, &unwritable).is_err(), "the output cannot be written");
    let fresh_store = dir.path().join("fresh-native");
    let (_, fresh, _) = reach_build(
        &record_check(REACH_APP, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: fresh_store.clone(),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert!(same_records(&options.store, &fresh_store), "the output failure kept the fresh record");
    let (built, bytes, status) = reach_build(&product, &options);
    assert_eq!(status, Some(6));
    assert!(bytes == fresh, "a fresh store's executable");
    assert_eq!(
        (built.reach.instance_path, built.compiled, built.linked),
        (0, 0, false),
        "the kept record and executable are a hit"
    );
}

/// Copy a native store's files, its temporaries excluded.
fn copy_store(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
        }
    }
}

/// An edit's writer dies after each of its renames in turn: whatever it left
/// (objects, debug rows, instance records, the reach record), the next build
/// is a fresh store's and writes the fresh record. Buck target:
/// core-driver-native.
#[test]
#[ignore = "explicit native integration target"]
fn reach_writer_crash_after_each_rename_then_builds_fresh() {
    let core = core_sources();
    let edited = REACH_APP.replace("5u8", "6u8");
    let dir = tempfile::tempdir().unwrap();
    let base = native_options(Backend::Cranelift, dir.path());
    reach_build(&record_check(REACH_APP, &core, dir.path(), "store"), &base);
    let fresh_store = dir.path().join("fresh-native");
    let (_, fresh, _) = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: fresh_store.clone(),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    let product = record_check(&edited, &core, dir.path(), "store");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for k in 0.. {
        let options = NativeOptions {
            store: dir.path().join(format!("crash-{k}")),
            output: dir.path().join(format!("crash-program-{k}")),
            ..native_options(Backend::Cranelift, dir.path())
        };
        copy_store(&base.store, &options.store);
        crate::store::CRASH.lock().unwrap().push((options.store.clone(), k));
        let crashed = product.build(&entry, &options).is_err();
        let left = {
            let mut crash = crate::store::CRASH.lock().unwrap();
            let at = crash.iter().position(|(r, _)| *r == options.store).unwrap();
            crash.remove(at).1
        };
        assert!(!crashed || left == 0, "crash after {k} renames: only the crash fails the build");
        let (_, bytes, status) = reach_build(&product, &options);
        assert_eq!(status, Some(7), "crash after {k} renames");
        assert!(bytes == fresh, "crash after {k} renames: a fresh store's executable");
        assert!(same_records(&options.store, &fresh_store), "crash after {k} renames: the fresh record");
        if left > 0 {
            assert!(k > 0, "the edit renames something");
            break;
        }
    }
}

/// Build `files` as project `app` into `store`, like [`reach_files_build`],
/// with the app's sources listed in reverse when `reversed`: the executable's
/// bytes and its run's exit and stderr.
fn descriptor_build(dir: &Path, files: &[(&str, &str)], store: &str, backend: Backend, optimize: bool, reversed: bool) -> (Vec<u8>, (Option<i32>, Vec<u8>)) {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join("app");
    if project.exists() {
        std::fs::remove_dir_all(&project).unwrap();
    }
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    for (name, source) in files {
        std::fs::write(project.join(name), source).unwrap();
    }
    let mut request = CheckRequest::program(&project, &libraries, &dir.join(store).join("check")).unwrap();
    if reversed {
        request.inputs.packages.last_mut().unwrap().sources.reverse();
    }
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join("cache"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let mut options = native_options(backend, dir);
    options.optimize = optimize;
    options.store = dir.join(store).join("native");
    options.output = dir.join(store).join("program");
    let built = product.build(&DefPath::from_full("app@1/main/main").unwrap(), &options).unwrap();
    let output = Command::new(&built.executable).output().unwrap();
    (std::fs::read(&built.executable).unwrap(), (output.status.code(), output.stderr))
}

/// One descriptor per function per program, and nothing observable depends
/// on where it lies or in which order it was emitted. One function is
/// described in two modules, in two instances of a generic body and through a
/// record, each its own unit; the entry checks that every such StaticFn is one
/// word, names one identity and calls one function, and that another
/// function's StaticFn differs (exit 0 when all hold, else the number of the
/// first that fails). Shapes: Cranelift dev, LLVM dev and LLVM release. Steps
/// over one primed store: the program, then one module edited (mixed: its
/// records rebuilt, the others served). Each step is built again in an empty
/// store, and once more with the app's sources listed in reverse. Oracle: the
/// empty store's executable and exit 0; the reversed build's executable is
/// byte for byte the same. Mutant: the descriptor defined local to each unit
/// (Cranelift `Linkage::Local`), red (exit 1: two words for one function).
#[test]
#[ignore = "explicit native integration target"]
fn reach_code_descriptor_is_one_per_function() {
    const A: &str = "pub fn f(x: u64) -> u64 {\n    x + 1\n}\npub fn g(x: u64) -> u64 {\n    x + 2\n}\npub fn here() -> StaticFn[fn(u64) -> u64] {\n    code.describe(f)\n}\n";
    const B: &str = "pub type Held {\n    s: StaticFn[fn(u64) -> u64]\n}\npub fn there() -> StaticFn[fn(u64) -> u64] {\n    code.describe(a.f)\n}\npub fn held() -> Held {\n    Held(s: code.describe(a.f))\n}\npub fn inside[F](s: StaticFn[F]) -> StaticFn[fn(u64) -> u64] {\n    code.describe(a.f)\n}\npub fn same[F](s: StaticFn[F], t: StaticFn[F]) -> bool {\n    s == t\n}\npub fn named[F](s: StaticFn[F], t: StaticFn[F]) -> bool {\n    code.identity(s) == code.identity(t)\n}\n";
    const MAIN: &str = "fn h(x: u8) -> u8 {\n    x\n}\nfn call(s: StaticFn[fn(u64) -> u64]) -> u64 {\n    let c = code.callable(s)\n    c(1)\n}\npub fn main() -> Result[(), Exit] {\n    let s = code.describe(a.f)\n    let g = code.describe(a.g)\n    if !(b.same(s, a.here())) {\n        return Error(Exit(status: 1u8))\n    }\n    if !(b.same(a.here(), b.there())) {\n        return Error(Exit(status: 2u8))\n    }\n    if !(b.same(s, b.inside(g))) {\n        return Error(Exit(status: 3u8))\n    }\n    if !(b.same(b.there(), b.inside(code.describe(h)))) {\n        return Error(Exit(status: 4u8))\n    }\n    if !(b.same(s, b.held().s)) {\n        return Error(Exit(status: 5u8))\n    }\n    if !(b.named(s, b.inside(code.describe(h)))) {\n        return Error(Exit(status: 6u8))\n    }\n    if !(!b.same(s, g) && !b.named(s, g)) {\n        return Error(Exit(status: 7u8))\n    }\n    if !(call(b.inside(g)) == 2 && call(g) == 3) {\n        return Error(Exit(status: 8u8))\n    }\n    if !(b.same(code.describe(h), code.describe(h))) {\n        return Error(Exit(status: 9u8))\n    }\n    Ok(())\n}\n";
    let edited = format!("{B}pub fn spare() -> u64 {{\n    7\n}}\n");
    let steps: [&[(&str, &str)]; 2] = [
        &[("a.hirsel", A), ("b.hirsel", B), ("main.hirsel", MAIN)],
        &[("a.hirsel", A), ("b.hirsel", &edited), ("main.hirsel", MAIN)],
    ];
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        let dir = tempfile::tempdir().unwrap();
        for (n, files) in steps.iter().enumerate() {
            let case = format!("{backend:?} release={optimize} step {n}");
            let (primed, ran) = descriptor_build(dir.path(), files, "primed", backend, optimize, false);
            let (fresh, expected) = descriptor_build(dir.path(), files, &format!("fresh{n}"), backend, optimize, false);
            let (reversed, reversed_ran) = descriptor_build(dir.path(), files, &format!("reversed{n}"), backend, optimize, true);
            assert_eq!(expected.0, Some(0), "{case}: {}", String::from_utf8_lossy(&expected.1));
            assert_eq!(ran, expected, "{case}: what the primed build reports");
            assert_eq!(reversed_ran, expected, "{case}: what the reversed build reports");
            assert!(primed == fresh, "{case}: the primed store's executable is a fresh store's");
            assert!(reversed == fresh, "{case}: the reversed sources' executable is the listed sources'");
        }
    }
}
