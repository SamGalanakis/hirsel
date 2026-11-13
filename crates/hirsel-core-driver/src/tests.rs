use super::*;
use hirsel_core_ir::{FuncId, eval};

fn request(source: &str, dir: &Path) -> CheckRequest {
    let source_file = dir.join("main.hirsel");
    std::fs::write(&source_file, source).unwrap();
    CheckRequest {
        inputs: CorpusManifest {
            core: package("core"),
            packages: vec![PackageManifest {
                package: package("app"),
                sources: vec![hirsel_core_resolve::ModuleSource {
                    file: source_file,
                    modules: vec!["main".into()],
                    signatures: false,
                }],
                dependencies: vec![],
            }],
        },
        store: dir.join("store"),
    }
}
#[test]
fn publishes_a_checked_body_and_disk_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn answer() -> u64 { 6 * 7 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let d = product.definitions.values().next().unwrap();
    let value = eval::run(&d.ir, d.check.function.unwrap(), &[], 1_000_000).outcome;
    assert_eq!(value, eval::Outcome::Returned(vec![42]));
    let store = Store::new(&dir.path().join("store")).unwrap();
    let root: Root = bincode::deserialize(&store.get(product.root).unwrap().unwrap()).unwrap();
    let identities: NameIndex =
        bincode::deserialize(&store.get(root.identities).unwrap().unwrap()).unwrap();
    assert_eq!(
        identities.module_path(product.files[0].module).modules,
        ["main"]
    );
    let file: FileProduct =
        bincode::deserialize(&store.get(root.files[0]).unwrap().unwrap()).unwrap();
    assert_eq!(file.syntax.text(), "fn answer() -> u64 { 6 * 7 }\n");
    let disk: DiskCheck =
        bincode::deserialize(&store.get(root.definitions[0].1).unwrap().unwrap()).unwrap();
    let ir = text::parse(&disk.ir).unwrap();
    assert_eq!(
        eval::run(&ir, FuncId(disk.function.unwrap()), &[], 1_000_000).outcome,
        value
    );
}
#[test]
fn refused_body_is_published_and_unreached_demand_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn bad() -> bool { let x = 7; true }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.accepted());
    assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-11"));
    assert!(
        product
            .definitions
            .values()
            .next()
            .unwrap()
            .check
            .function
            .is_none()
    );
    let absent = DefPath(hirsel_core_ids::ItemPath {
        package: package("app"),
        modules: vec!["absent".into()],
        name: "missing".into(),
    });
    assert!(
        product
            .demand(&absent)
            .err()
            .unwrap()
            .to_string()
            .contains("outside reached")
    );
}
#[test]
fn damaged_disk_bytes_are_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let key = store.put(b"published payload").unwrap();
    let good = std::fs::read(store.path(key)).unwrap();
    for offset in [0, 8, 12, 20, 52] {
        let mut damaged = good.clone();
        damaged[offset] ^= 1;
        std::fs::write(store.path(key), damaged).unwrap();
        assert!(store.get(key).unwrap().is_none());
    }
    std::fs::write(store.path(key), &good[..good.len() - 1]).unwrap();
    assert!(store.get(key).unwrap().is_none());
}

#[test]
fn unrelated_parse_failure_does_not_suppress_a_valid_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u64 { 42 }\n", dir.path());
    let bad = dir.path().join("broken.hirsel");
    std::fs::write(&bad, "fn broken(\n").unwrap();
    req.inputs.packages[0]
        .sources
        .push(hirsel_core_resolve::ModuleSource {
            file: bad,
            modules: vec!["broken".into()],
            signatures: false,
        });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    let answer = DefPath(hirsel_core_ids::ItemPath {
        package: package("app"),
        modules: vec!["main".into()],
        name: "answer".into(),
    });
    let d = product.demand(&answer).unwrap();
    assert_eq!(
        eval::run(&d.ir, d.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}

fn names(product: &CheckProduct) -> Vec<&str> {
    let mut names: Vec<_> = product
        .rechecked
        .iter()
        .map(|p| p.0.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn warm_and_body_edits_reuse_signature_readers_in_memory_and_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = "fn callee() -> u64 { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    let first = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&first), ["callee", "caller", "independent"]);
    let warm = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(warm.rechecked.is_empty());
    let b = a.replace("{ 1 }", "{ 2 }");
    let edit = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edit), ["callee"]);
    let callee = edit
        .definitions
        .values()
        .find(|d| d.definition.0.name == "callee")
        .unwrap();
    assert_eq!(
        eval::run(&callee.ir, callee.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![2])
    );
    let old_caller = first
        .definitions
        .values()
        .find(|d| d.definition.0.name == "caller")
        .unwrap();
    let new_caller = edit.definitions.get(&old_caller.definition).unwrap();
    assert!(Arc::ptr_eq(old_caller, new_caller));
    let disk_warm = Driver::default()
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(disk_warm.rechecked.is_empty());
    let disk_edit = Driver::default()
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&disk_edit), ["callee"]);
}
#[test]
fn signature_edits_recheck_readers_and_leave_independent_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = "fn callee() -> u64 { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    let b = a
        .replace("callee() -> u64", "callee() -> bool")
        .replace("{ 1 }", "{ true }");
    let edited = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["callee", "caller"]);
    assert!(!edited.accepted());
    assert!(edited.diagnostics.iter().any(|r| r.primary.item == 1));
    let reused = Driver::default()
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(reused.rechecked.is_empty());
    assert_eq!(
        encode(&edited.diagnostics).unwrap(),
        encode(&reused.diagnostics).unwrap()
    );
}
#[test]
fn negative_and_shadowed_name_reads_invalidate_on_new_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a =
        "fn local() -> u64 { let invented = 42u64; invented }\nfn bad() -> u64 { let n = 7; 0 }\n";
    let old = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&old), ["bad", "local"]);
    let mut req = request(a, dir.path());
    let extra = dir.path().join("invented.hirsel");
    std::fs::write(&extra, "fn value() -> u64 { 3 }\n").unwrap();
    req.inputs.packages[0]
        .sources
        .push(hirsel_core_resolve::ModuleSource {
            file: extra,
            modules: vec!["invented".into()],
            signatures: false,
        });
    let edited = driver.check(req, &mut Meter::new(false)).unwrap();
    assert!(
        edited
            .diagnostics
            .iter()
            .any(|r| r.rule == "LANG-7" && r.message.contains("invented"))
    );
    assert!(!names(&edited).contains(&"bad"));
    assert!(names(&edited).contains(&"value"));
}
#[test]
fn transitive_type_reads_and_discovery_order_have_stable_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = "type Number = u64\nfn callee() -> Number { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    let old = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(old.accepted(), "{:?}", old.diagnostics);
    let b = a.replace("Number = u64", "Number = u32");
    let edited = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["callee", "caller"]);
    let mut req = request(&b, dir.path());
    req.inputs.packages.reverse();
    req.inputs.packages[0].sources.reverse();
    let reversed = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert_eq!(edited.root, reversed.root);
    assert_eq!(
        encode(&edited.diagnostics).unwrap(),
        encode(&reversed.diagnostics).unwrap()
    );
}

#[test]
fn resident_results_are_trusted_and_damaged_disk_results_recompute() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let source = "fn answer() -> u64 { 42 }\n";
    let first = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    let root: Root = decode(&store.get(first.root).unwrap().unwrap()).unwrap();
    std::fs::write(store.path(root.definitions[0].1), b"damaged").unwrap();
    let resident = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(resident.accepted());
    assert!(resident.rechecked.is_empty());
    let disk = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(disk.accepted());
    assert_eq!(names(&disk), ["answer"]);
}

#[test]
fn adding_a_definition_rebinds_cached_refusal_sites_without_checking_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let old = "fn first() -> u64 { 1 }\nfn bad() -> u64 { let n = 7; 0 }\n";
    driver
        .check(request(old, dir.path()), &mut Meter::new(false))
        .unwrap();
    let new = old.replace("fn bad()", "fn inserted() -> u64 { 2 }\nfn bad()");
    let result = driver
        .check(request(&new, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&result), ["inserted"]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.rule == "LANG-11" && d.primary.item == 2)
    );
}
