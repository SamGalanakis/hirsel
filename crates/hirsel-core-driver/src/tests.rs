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
    let cache = Cache::open(&store, product.root).unwrap().unwrap();
    let file: FileProduct = decode(cache.root.files[0].bytes(&cache.bytes).unwrap()).unwrap();
    assert_eq!(file.syntax.text(), "fn answer() -> u64 { 6 * 7 }\n");
    let disk: DiskCheck = decode(
        cache
            .root
            .definitions
            .values()
            .next()
            .unwrap()
            .bytes(&cache.bytes)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        eval::run(&disk.ir, FuncId(disk.function.unwrap()), &[], 1_000_000).outcome,
        value
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("store")).unwrap().count(),
        2
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
    let absent = ModuleId {
        package: package("app"),
        modules: vec!["absent".into()],
    }
    .definition("missing");
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
    for offset in [0, 8, 12, good.len() - 40, good.len() - 1] {
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
    let answer = ModuleId {
        package: package("app"),
        modules: vec!["main".into()],
    }
    .definition("answer");
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
    let a = "fn local() -> u64 { let invented = 42u64; invented }\nfn missing() -> u64 { future() }\nfn bad() -> u64 { let n = 7; 0 }\n";
    let old = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&old), ["bad", "local", "missing"]);
    let b = format!("{a}fn invented() -> u64 {{ 3 }}\nfn future() -> u64 {{ 9 }}\n");
    let edited = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["future", "invented", "local", "missing"]);
    assert!(
        edited
            .diagnostics
            .iter()
            .any(|r| r.rule == "LANG-7" && r.message.contains("invented"))
    );
    let local = edited
        .definitions
        .values()
        .find(|d| d.definition.0.name == "local")
        .unwrap();
    assert_ne!(
        old.definitions[&local.definition].key.lookups,
        local.key.lookups
    );
    let missing = edited
        .definitions
        .values()
        .find(|d| d.definition.0.name == "missing")
        .unwrap();
    assert!(missing.check.refusals.is_empty());
    assert!(missing.check.function.is_some());
}
#[test]
fn transitive_type_reads_and_discovery_order_have_stable_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let other = dir.path().join("other.hirsel");
    std::fs::write(&other, "fn extra() -> u64 { 3 }\n").unwrap();
    let configured = |text: &str| {
        let mut req = request(text, dir.path());
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file: other.clone(),
                modules: vec!["other".into()],
                signatures: false,
            });
        req
    };
    let a = "type Number = u64\nfn callee() -> Number { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    let old = driver.check(configured(a), &mut Meter::new(false)).unwrap();
    assert!(old.accepted(), "{:?}", old.diagnostics);
    let b = a.replace("Number = u64", "Number = u32");
    let edited = driver
        .check(configured(&b), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["callee", "caller"]);
    let mut req = configured(&b);
    req.inputs.packages.reverse();
    req.inputs.packages[0].sources.reverse();
    req.store = dir.path().join("reverse-store");
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
    std::fs::write(store.path(first.root), b"damaged").unwrap();
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

#[test]
fn failed_archive_write_keeps_previous_root_and_removes_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let old = store.put(b"previous complete result").unwrap();
    store.publish_root(old).unwrap();
    let failure = store.archive(|writer| {
        use std::io::Write;
        writer.write_all(b"unfinished next result")?;
        Err(io::Error::other("injected publication failure"))
    });
    assert!(failure.is_err());
    assert_eq!(store.selection("check-root").unwrap(), Some(old));
    assert_eq!(
        store.get(old).unwrap().unwrap(),
        b"previous complete result"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn native_ir_tables_round_trip_without_text_reconstruction() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../hirsel-core-ir/samples/control.core"),
        include_str!("../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../hirsel-core-ir/samples/calls.core"),
        include_str!("../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../hirsel-core-ir/samples/custody.core"),
        include_str!("../../hirsel-core-ir/samples/variants.core"),
    ] {
        let ir = hirsel_core_ir::text::parse(source).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let mut at = None;
        let hash = store
            .archive(|writer| {
                at = Some(writer.record(&ir)?);
                Ok(())
            })
            .unwrap();
        let bytes = store.get(hash).unwrap().unwrap();
        let decoded: Module = decode(at.unwrap().bytes(&bytes).unwrap()).unwrap();
        assert_eq!(ir, decoded);
    }
    let dir = tempfile::tempdir().unwrap();
    let source = "fn f(x: u64) -> u64 { let mut n: u64 = 0; while n < x { n += 1 }; if n == x { n } else { 0 } }\n";
    let first = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(first.accepted(), "{:?}", first.diagnostics);
    let second = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(second.rechecked.is_empty());
    let a = first.definitions.values().next().unwrap();
    let b = second.definitions.values().next().unwrap();
    assert_eq!(a.ir, b.ir);
    assert_eq!(
        eval::run(&b.ir, b.check.function.unwrap(), &[17], 1_000_000).outcome,
        eval::Outcome::Returned(vec![17])
    );
}
