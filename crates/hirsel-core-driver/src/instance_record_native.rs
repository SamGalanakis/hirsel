// Cranelift development instance and entry records (M2). Oracle for every
// case: a build of the same sources into an empty store, from the same source
// files (debug lines name them).
fn record_request(app: &str, core: &Path, dir: &Path, store: &str) -> CheckRequest {
    let mut request = source_request(app, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(core, package("core"), true).unwrap();
    request.store = dir.join(store);
    request
}
fn record_check(app: &str, core: &Path, dir: &Path, store: &str) -> Arc<CheckProduct> {
    let product = Driver::default()
        .check(record_request(app, core, dir, store), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    product
}
fn core_sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core")
}
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        // Test sources may be read-only links; the copy is a plain, writable file.
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::write(target, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}
/// The entry records a native store holds.
fn entry_records(root: &Path) -> usize {
    let store = Store::new(root).unwrap();
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let bytes = (0..32)
                .map(|i| u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            Some(ContentHash(bytes.try_into().ok()?))
        })
        .filter(|key| entry_record(&store, *key).unwrap().is_some())
        .count()
}
fn executable_bytes(product: &CheckProduct, options: &NativeOptions) -> (Vec<u8>, Option<i32>) {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, options).unwrap();
    let status = Command::new(&built.executable).status().unwrap().code();
    (std::fs::read(&built.executable).unwrap(), status)
}

/// Law: the entry record is keyed by the entry body's record address, which
/// commits to the entry module's `Result` and `Exit` rows the ABI is read
/// from. A source-backed core widens `Exit`'s alignment with a new field, so
/// `Result`'s error payload, and the status in it, moves; the program's text
/// is unchanged. In a primed store the entry record misses, the status is
/// read at its new offset, and the executable equals a fresh store's, on both
/// engines. Mutant: the entry key without the address, a
/// stale exit offset, red on the status and the bytes.
fn exit_layout_misses_the_entry_record(backend: Backend) {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(exit_with(7u8)) }\n";
    const HELPER: &str = "\npub fn exit_with(status: u8) -> Exit { Exit(status: status) }\n";
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("core-sources");
    copy_tree(&core_sources(), &core);
    let prelude = std::fs::read_to_string(core.join("core.hirsel")).unwrap();
    std::fs::write(core.join("core.hirsel"), format!("{prelude}{HELPER}")).unwrap();
    let options = native_options(backend, dir.path());
    let primed = executable_bytes(&record_check(APP, &core, dir.path(), "store"), &options);
    assert_eq!(primed.1, Some(7), "{backend:?}: primed");
    let before = entry_records(&options.store);
    let moved = prelude.replace(
        "pub type Exit: key {\n    status: u8\n}",
        "pub type Exit: key {\n    status: u8,\n    pad: u64\n}",
    );
    assert_ne!(moved, prelude, "the prelude declares Exit");
    std::fs::write(
        core.join("core.hirsel"),
        format!(
            "{moved}{}",
            HELPER.replace("Exit(status: status)", "Exit(status: status, pad: 0u64)")
        ),
    )
    .unwrap();
    let rebuilt = executable_bytes(&record_check(APP, &core, dir.path(), "store"), &options);
    if matches!(backend, Backend::Cranelift) {
        assert_eq!(entry_records(&options.store), before + 1, "the entry record misses");
    }
    let fresh = executable_bytes(
        &record_check(APP, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            ..native_options(backend, dir.path())
        },
    );
    assert_eq!(rebuilt.1, Some(7), "{backend:?}: moved status");
    assert!(rebuilt == fresh, "{backend:?}: the rebuilt executable is a fresh store's");
}
#[test]
#[ignore = "explicit native integration target"]
fn exit_layout_misses_the_entry_record_cranelift() {
    exit_layout_misses_the_entry_record(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn exit_layout_misses_the_entry_record_llvm() {
    exit_layout_misses_the_entry_record(Backend::Llvm);
}

/// Law: a body record whose IR is damaged at the same length keeps a valid
/// head, so check reuses it; the first request that reads the IR refuses
/// with an error naming the record, and removes it. The next request checks
/// the body again and builds a fresh store's executable. Mutant: the error
/// without the removal, red on the file and on request 2.
#[test]
#[ignore = "explicit native integration target"]
fn same_length_ir_damage_refuses_once_then_builds_fresh() {
    const APP: &str = "fn get(x: u8) -> u8 { x + 1u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(6u8))) }\n";
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let lines = |debug_lines| NativeOptions {
        debug_lines,
        ..native_options(Backend::Cranelift, dir.path())
    };
    executable_bytes(&record_check(APP, &core, dir.path(), "store"), &lines(false));
    let store = Store::new(&dir.path().join("store")).unwrap();
    let get = DefPath::from_full("app@1/main/get").unwrap();
    let (_, record) = check_index::ProgramIndex::open(&store)
        .unwrap()
        .live(&get)
        .unwrap();
    let mut bytes = std::fs::read(store.path(record)).unwrap();
    let last = bytes.len() - 33;
    bytes[last] ^= 1;
    std::fs::write(store.path(record), bytes).unwrap();
    // Request 1: the head is intact, so check reuses `get`; an LLVM build
    // has no object in this store, so it reads `get`'s IR.
    let first = record_check(APP, &core, dir.path(), "store");
    assert!(first.rechecked.is_empty(), "{:?}", first.rechecked);
    let error = first
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(Backend::Llvm, dir.path()),
        )
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains(&format!(
            "the body record {} of `{get}` is damaged behind its head",
            store.path(record).display()
        )),
        "{error}"
    );
    assert!(!store.path(record).exists(), "request 1 removes the damaged record");
    let second = record_check(APP, &core, dir.path(), "store");
    assert!(second.rechecked.contains(&get), "{:?}", second.rechecked);
    let rebuilt = executable_bytes(&second, &lines(true));
    let fresh = executable_bytes(
        &record_check(APP, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            ..lines(true)
        },
    );
    assert_eq!(rebuilt.1, Some(7));
    assert!(rebuilt == fresh, "request 2 builds a fresh store's executable");
}

/// Law (985 a): every link-plan input the environment does not cover is in
/// the entry key's `policy` and in the plan the executable key takes: the
/// root limit, the grant mask, the ordered directories and the charge-report
/// path. One body, one store: each input changed in turn builds a fresh
/// store's executable. The charge-report path needs the observed runtime, so
/// its case is in //:core-memory-report. The reach record's selection and the
/// executable key take the engine, optimisation and debug rows too (R5), and
/// the instance limit: a record from a complete walk is not used under a
/// limit that walk exceeds. Mutant: a plan input left out of the plan's
/// encoding, a stale executable, red; the limit left out of the selection, a
/// build where fresh stops, red.
#[test]
#[ignore = "explicit native integration target"]
fn each_plan_input_builds_a_fresh_stores_executable() {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let base = || native_options(Backend::Cranelift, dir.path());
    let [a, b] = ["a", "b"].map(|name| {
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        path.into_os_string().into_string().unwrap()
    });
    let variants: Vec<(&str, NativeOptions)> = vec![
        ("base", base()),
        ("root limit", NativeOptions { root_limit: 2_097_152, ..base() }),
        ("grant mask", NativeOptions { grant_mask: 1 << grant_kind("Clock").unwrap(), ..base() }),
        ("directories", NativeOptions { directories: vec![a.clone(), b.clone()], ..base() }),
        ("directory order", NativeOptions { directories: vec![b.clone(), a.clone()], ..base() }),
        ("optimize", NativeOptions { optimize: true, ..base() }),
        ("debug lines", NativeOptions { debug_lines: true, ..base() }),
        ("backend", NativeOptions { backend: Backend::Llvm, ..base() }),
        ("output path", NativeOptions { output: dir.path().join("elsewhere"), ..base() }),
        ("base again", base()),
    ];
    let shared = record_check(APP, &core, dir.path(), "store");
    for (n, (name, options)) in variants.into_iter().enumerate() {
        let built = executable_bytes(&shared, &options);
        let fresh = executable_bytes(
            &record_check(APP, &core, dir.path(), &format!("fresh{n}")),
            &NativeOptions {
                store: dir.path().join(format!("fresh-native{n}")),
                ..options
            },
        );
        assert_eq!(built.1, Some(7), "{name}: runs");
        assert!(built == fresh, "{name}: the shared store's executable is a fresh store's");
    }
    // A record from a complete walk is not used under a limit the walk
    // exceeds: the build stops where a fresh store's stops.
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let limited = || NativeOptions { instance_limit: 0, ..base() };
    assert!(shared.build(&entry, &limited()).is_err(), "instance limit: the shared store stops");
    let fresh = record_check(APP, &core, dir.path(), "fresh-limit");
    assert!(
        fresh
            .build(&entry, &NativeOptions { store: dir.path().join("fresh-native-limit"), ..limited() })
            .is_err(),
        "instance limit: a fresh store stops"
    );
}

/// The instance records a store holds, with their addresses.
fn instance_records(store: &Store, root: &Path) -> Vec<(ContentHash, crate::native_object::Instance)> {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let bytes = (0..32)
                .map(|i| u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            let address = ContentHash(bytes.try_into().ok()?);
            let record = decode::<crate::native_object::Instance>(&store.get(address).ok()??)?;
            Some((address, record))
        })
        .collect()
}

/// Law (§11.4, closure corruption): a hit's closures feed its children's
/// arguments, so a record is a hit only while each closure's rows hash to the
/// hash it is filed under. `get`'s record is forged to carry `Q`'s rows under
/// `P`'s closure hash, stored again under its key, and `echo[P]`'s
/// record is evicted: the forged parent misses, and the build equals a fresh
/// store's. Mutant: no rehash on decode, so `echo[P]` is materialised on
/// `Q`'s rows, red.
#[test]
#[ignore = "explicit native integration target"]
fn a_forged_closure_misses_its_parent() {
    const APP: &str = "type P { a: u8, b: u64 }\ntype Q { a: u8, b: u32 }\nfn echo[T: plain](take x: T) -> T { x }\nfn get() -> u8 { echo(P(a: 7u8, b: 1u64)).a + echo(Q(a: 0u8, b: 2u32)).a }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get())) }\n";
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let options = native_options(Backend::Cranelift, dir.path());
    let primed = record_check(APP, &core, dir.path(), "store");
    assert_eq!(executable_bytes(&primed, &options).1, Some(7));
    let store = Store::new(&options.store).unwrap();
    let records = instance_records(&store, &options.store);
    let get = DefPath::from_full("app@1/main/get").unwrap();
    let echo = DefPath::from_full("app@1/main/echo").unwrap();
    let (_, mut parent) = records
        .iter()
        .find(|(_, r)| r.identity.definition == get)
        .map(|(a, r)| (*a, decode::<crate::native_object::Instance>(&encode(r).unwrap()).unwrap()))
        .unwrap();
    assert_eq!(parent.closures.len(), 2, "get argues echo with P and with Q");
    let child = |hash: ContentHash| {
        records
            .iter()
            .find(|(_, r)| r.identity.definition == echo && r.identity.closures == [hash])
            .map(|(a, _)| *a)
    };
    // The closure whose child is echo[P]: P's rows are the longer layout.
    let (p, q) = (parent.closures[0].clone(), parent.closures[1].clone());
    let (p, q) = if child(p.hash).is_some() { (p, q) } else { (q, p) };
    parent.closures = parent
        .closures
        .iter()
        .map(|c| {
            if c.hash == p.hash {
                Arc::new(hirsel_core_ir::Closure { rows: q.rows.clone(), hash: p.hash })
            } else {
                Arc::clone(c)
            }
        })
        .collect();
    let inputs = native_inputs(&primed, &options).unwrap();
    let key = inputs
        .records
        .as_ref()
        .unwrap()
        .key(&parent.identity, parent.own)
        .unwrap();
    store.put_keyed(key, &encode(&parent).unwrap()).unwrap();
    std::fs::remove_file(store.path(child(p.hash).unwrap())).unwrap();
    let rebuilt = executable_bytes(&record_check(APP, &core, dir.path(), "store"), &options);
    let fresh = executable_bytes(
        &record_check(APP, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert_eq!(rebuilt.1, Some(7));
    assert!(rebuilt == fresh, "the forged parent misses and the build is a fresh store's");
}

/// A two-package request: `app` (main.hirsel) depends on `lib` (api.hirsel)
/// and both on the source core.
fn library_request(app: &str, lib: &str, core: &Path, dir: &Path, store: &str) -> CheckRequest {
    let mut request = record_request(app, core, dir, store);
    let file = dir.join("api.hirsel");
    std::fs::write(&file, lib).unwrap();
    request.inputs.packages[0].dependencies.push(package("lib"));
    request.inputs.packages.push(PackageManifest {
                    namespace: None,
        package: package("lib"),
        sources: vec![hirsel_core_resolve::ModuleSource {
                    foreign: false,
                    captured: None,
            file,
            modules: vec!["api".into()],
            signatures: false,
        }],
        dependencies: vec![package("core")],
    });
    request
}
fn library_check(app: &str, lib: &str, core: &Path, dir: &Path, store: &str) -> Arc<CheckProduct> {
    Driver::default()
        .check(library_request(app, lib, core, dir, store), &mut Meter::new(false))
        .unwrap()
}
/// The diagnostics a build of `app@1/main/main` stops on: rule, module and
/// file span, sorted.
fn build_diagnostics(product: &CheckProduct, options: &NativeOptions) -> Vec<(String, String, u32, u32)> {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let reached = product.reachable_for(&entry, options).unwrap();
    let mut found: Vec<_> = reached
        .diagnostics
        .iter()
        .map(|d| {
            let site = product.source_site(&d.primary).unwrap();
            (d.rule.clone(), site.module, site.span.start, site.span.end)
        })
        .collect();
    found.sort();
    found
}
fn malformed_literal(start: u32) -> Vec<(String, String, u32, u32)> {
    ["LANG-3", "LANG-5"]
        .iter()
        .map(|rule| (rule.to_string(), "lib@1/api".to_string(), start, start + 5))
        .collect()
}
const PROBE_MAIN: &str =
    "pub fn main() -> Result[(), Exit] { if probe() == 7u64 { Ok(()) } else { Error(Exit(status: 91u8)) } }\n";

/// Law: a build reports the diagnostics of every library body it reaches,
/// also behind an instance walk that stopped. An edit makes the malformed
/// `lib.api.broken` reachable through the app's `probe`; the walk stops at
/// `probe`, and only the stopped-walk fallback in reachability reaches
/// `broken`. In a store primed by the valid build, and in a fresh one, the
/// build stops on LANG-3 and LANG-5 at `0u128` (bytes 21..26 of api.hirsel,
/// counted by hand). Mutant: no fallback (`if false`), red: no diagnostics.
fn new_reachable_diagnostic(backend: Backend) {
    const LIB: &str = "pub fn broken()->u64{0u128}\n";
    let before = format!("fn probe() -> u64 {{ 7 }}\n{PROBE_MAIN}");
    let after = format!("fn probe() -> u64 {{ lib.api.broken() }}\n{PROBE_MAIN}");
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let options = native_options(backend, dir.path());
    let primed = library_check(&before, LIB, &core, dir.path(), "store");
    assert_eq!(executable_bytes(&primed, &options).1, Some(0), "{backend:?}: primed");
    let edited = build_diagnostics(&library_check(&after, LIB, &core, dir.path(), "store"), &options);
    let fresh_options = NativeOptions {
        store: dir.path().join("fresh-native"),
        ..native_options(backend, dir.path())
    };
    let fresh = build_diagnostics(&library_check(&after, LIB, &core, dir.path(), "fresh"), &fresh_options);
    assert_eq!(edited, malformed_literal(21), "{backend:?}: primed store");
    assert_eq!(fresh, malformed_literal(21), "{backend:?}: fresh store");
}
#[test]
#[ignore = "explicit native integration target"]
fn new_reachable_diagnostic_cranelift() {
    new_reachable_diagnostic(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn new_reachable_diagnostic_llvm() {
    new_reachable_diagnostic(Backend::Llvm);
}

/// Law: a library body rejected in a store is rebuilt from its repaired
/// source. The app's `probe` calls `lib.api.get`, whose `0u128` is malformed:
/// the build stops on LANG-3 and LANG-5 at bytes 18..23 of api.hirsel
/// (counted by hand), reached only behind the stopped walk. After the repair
/// to `7`, the build in that store runs `main` to status 0 and its executable
/// equals a fresh store's. Mutant: no stopped-walk fallback, red: no
/// diagnostics before the repair.
fn rejected_repair(backend: Backend) {
    let app = format!("fn probe() -> u64 {{ lib.api.get() }}\n{PROBE_MAIN}");
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let options = native_options(backend, dir.path());
    let rejected = library_check(&app, "pub fn get()->u64{0u128}\n", &core, dir.path(), "store");
    assert_eq!(build_diagnostics(&rejected, &options), malformed_literal(18), "{backend:?}: rejected");
    const REPAIRED: &str = "pub fn get()->u64{7}\n";
    let repaired = executable_bytes(&library_check(&app, REPAIRED, &core, dir.path(), "store"), &options);
    let fresh = executable_bytes(
        &library_check(&app, REPAIRED, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            ..native_options(backend, dir.path())
        },
    );
    assert_eq!(repaired.1, Some(0), "{backend:?}: repaired");
    assert!(repaired == fresh, "{backend:?}: the repaired build is a fresh store's");
}
#[test]
#[ignore = "explicit native integration target"]
fn rejected_repair_cranelift() {
    rejected_repair(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn rejected_repair_llvm() {
    rejected_repair(Backend::Llvm);
}

/// How a stored file is damaged on disk.
#[derive(Clone, Copy, Debug)]
enum Damage {
    /// One flipped bit inside the object's code: a different instruction.
    Text,
    /// One flipped bit in the middle of the file.
    Middle,
    /// The second half removed.
    Truncate,
}
/// The objects a native store holds: its ELF files (a record starts with the
/// store's magic).
fn stored_objects(root: &Path) -> Vec<PathBuf> {
    let mut objects: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| std::fs::read(p).is_ok_and(|b| b.starts_with(b"\x7fELF")))
        .collect();
    objects.sort();
    objects
}
fn damage(path: &Path, how: Damage) {
    use object::{Object, ObjectSection};
    let mut bytes = std::fs::read(path).unwrap();
    match how {
        Damage::Text => {
            let at = object::File::parse(&*bytes)
                .unwrap()
                .sections()
                .filter(|s| s.name().is_ok_and(|n| n.starts_with(".text")))
                .filter_map(|s| s.file_range())
                .find(|&(_, size)| size > 0)
                .map(|(start, size)| (start + size / 2) as usize);
            let Some(at) = at else { return };
            bytes[at] ^= 1;
        }
        Damage::Middle => {
            let at = bytes.len() / 2;
            bytes[at] ^= 1;
        }
        Damage::Truncate => bytes.truncate(bytes.len() / 2),
    }
    std::fs::write(path, bytes).unwrap();
}
const DAMAGE_APP: &str = "fn get(x: u8) -> u8 { x + 1u8 }\nfn base() -> u8 { 5u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(base()))) }\n";
const DAMAGE_EDIT: &str = "fn get(x: u8) -> u8 { x + 1u8 }\nfn base() -> u8 { 6u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(base()))) }\n";

/// Law: a stored object that fails its check is replaced by the bytes
/// emitted again, so the link reads a fresh store's object. Every object of a
/// primed store is damaged (a code bit, a middle bit, a truncation), then one
/// body is edited: the executable is a fresh store's, byte for byte, and the
/// next request is a plain hit. Bug caught (red at d9db58c8): `put_bare`
/// kept the damaged file because its path existed, so the edit linked it,
/// exited 0 with a wrong executable, and cached that executable; a damaged
/// symbol table failed every later link.
fn damaged_objects_build_a_fresh_stores_executable(backend: Backend) {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for how in [Damage::Text, Damage::Middle, Damage::Truncate] {
        let dir = tempfile::tempdir().unwrap();
        let core = core_sources();
        let options = native_options(backend, dir.path());
        executable_bytes(&record_check(DAMAGE_APP, &core, dir.path(), "store"), &options);
        let objects = stored_objects(&options.store);
        assert!(!objects.is_empty());
        for object in &objects {
            damage(object, how);
        }
        let edited = record_check(DAMAGE_EDIT, &core, dir.path(), "store");
        let built = edited
            .build(&entry, &options)
            .unwrap_or_else(|e| panic!("{how:?}: {e}"));
        let status = Command::new(&built.executable).status().unwrap().code();
        let rebuilt = (std::fs::read(&built.executable).unwrap(), status);
        let fresh = executable_bytes(
            &record_check(DAMAGE_EDIT, &core, dir.path(), "fresh"),
            &NativeOptions {
                store: dir.path().join("fresh-native"),
                ..native_options(backend, dir.path())
            },
        );
        assert_eq!(rebuilt.1, Some(7), "{how:?}");
        assert!(rebuilt == fresh, "{how:?}: the executable is a fresh store's");
        let again = edited.build(&entry, &options).unwrap();
        assert_eq!((again.compiled, again.linked), (0, false), "{how:?}: a plain hit");
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn damaged_objects_build_a_fresh_stores_executable_cranelift() {
    damaged_objects_build_a_fresh_stores_executable(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn damaged_objects_build_a_fresh_stores_executable_llvm() {
    damaged_objects_build_a_fresh_stores_executable(Backend::Llvm);
}

/// Law: a reused executable is the store's verified record, so a damaged one
/// is linked again on a no-change request and the result is the first
/// build's executable. Guards the executable record's check on reuse.
fn damaged_executable_links_again(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let core = core_sources();
    let options = native_options(backend, dir.path());
    let product = record_check(DAMAGE_APP, &core, dir.path(), "store");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &options).unwrap();
    let status = Command::new(&built.executable).status().unwrap().code();
    let first = (std::fs::read(&built.executable).unwrap(), status);
    // The executable record is framed under the build's key; its detached
    // DWARF, also an ELF file, is a record of its own.
    let record = Store::new(&options.store).unwrap().path(built.identity);
    assert!(
        std::fs::read(&record).is_ok_and(|b| b.get(12..16) == Some(b"\x7fELF")),
        "the executable record"
    );
    damage(&record, Damage::Middle);
    let again = product.build(&entry, &options).unwrap();
    assert!(again.linked, "the damaged executable is not reused");
    let status = Command::new(&again.executable).status().unwrap().code();
    assert!((std::fs::read(&again.executable).unwrap(), status) == first);
    assert!(!product.build(&entry, &options).unwrap().linked, "then a plain hit");
}
#[test]
#[ignore = "explicit native integration target"]
fn damaged_executable_links_again_cranelift() {
    damaged_executable_links_again(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn damaged_executable_links_again_llvm() {
    damaged_executable_links_again(Backend::Llvm);
}
