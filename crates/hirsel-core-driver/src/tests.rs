use super::*;
use hirsel_core_ir::{FuncId, eval};

pub(crate) fn request(source: &str, dir: &Path) -> CheckRequest {
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
        library: LibraryInput::Sources,
        entry: None,
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
    let absent = DefPath::from_full("app@1/absent/missing").unwrap();
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
    let answer = DefPath::from_full("app@1/main/answer").unwrap();
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

fn library_request(source: &str, dir: &Path) -> CheckRequest {
    let mut req = request(source, dir);
    req.inputs.packages[0].package = package("core");
    req.inputs.packages[0].sources[0].modules = vec!["lib".into()];
    req
}
#[test]
fn image_signature_use_skips_source_and_body_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub type Number = u64\npub fn answer() -> Number { 42 }\n",
                dir.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    let produced = image.unwrap();
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    assert_eq!(opened.identity, produced.identity);
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
    std::fs::remove_file(dir.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn caller(x: lib.Number) -> u64 { x + lib.answer() }\n",
        app.path(),
    );
    req.library = LibraryInput::Image(Arc::clone(&opened));
    let checked = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        checked.accepted(),
        "{:?} {:?}",
        checked.diagnostics,
        checked.incomplete
    );
    assert_eq!(names(&checked), ["caller"]);
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
    let answer = DefPath::from_full("core@1/lib/answer").unwrap();
    let body = checked.demand_body(&answer).unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 1));
    assert!(
        opened
            .file(0)
            .unwrap()
            .syntax
            .text()
            .contains("pub type Number = u64")
    );
    assert_eq!(opened.decoded_files(), 1);
}
#[test]
fn corrupt_unused_image_body_is_a_miss_on_demand_only() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = image.unwrap();
    let answer = DefPath::from_full("core@1/lib/answer").unwrap();
    let hash = image.directory.archive;
    let store = Store::new(&dir.path().join("store")).unwrap();
    std::fs::write(store.path(hash), b"damaged").unwrap();
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn caller() -> u64 { 42 }\n", app.path());
    req.library = LibraryInput::Image(Arc::clone(&opened));
    let checked = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(checked.accepted(), "{:?}", checked.diagnostics);
    assert!(
        opened
            .demand(&answer)
            .err()
            .unwrap()
            .to_string()
            .contains("cache miss")
    );
    let missing = DefPath::from_full("core@1/absent/absent").unwrap();
    assert!(
        opened
            .demand(&missing)
            .err()
            .unwrap()
            .to_string()
            .contains("outside reached")
    );
}
#[test]
fn refused_library_never_publishes_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let (product, image) = Driver::default()
        .produce_image(
            library_request("pub fn bad() -> u64 { true }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.diagnostics.is_empty());
    assert!(image.is_none());
    assert!(
        CoreImage::open(&dir.path().join("store"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn machine_refusal_spans_point_into_current_file_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let source = "// leading comment\nfn good() -> u64 { 42 }\nfn bad() -> u64 { let n = 7; 0 }\n";
    let result = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let diagnostics = result.machine_diagnostics().unwrap();
    let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
    assert_eq!(refusal.primary.module, "app@1/main");
    let span = refusal.primary.span;
    assert_eq!(&source[span.start as usize..span.end as usize], "7");
    let json = serde_json::to_value(refusal).unwrap();
    for key in ["rule", "primary", "causal", "declared", "required"] {
        assert!(json.get(key).is_some());
    }
}
#[test]
fn instance_collection_is_finite_deduplicated_and_order_independent() {
    use hirsel_core_ir::InstanceKey;
    let definition = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    let key = |name: &str| InstanceKey {
        definition: definition(name),
        types: vec![],
        dictionaries: vec![],
    };
    let a = key("a");
    let b = key("b");
    let leaf = key("leaf");
    let edges = |k: &InstanceKey| {
        Ok(if *k == a || *k == b {
            vec![leaf.clone(), leaf.clone()]
        } else {
            vec![]
        })
    };
    assert_eq!(
        collect_instances([a.clone(), b.clone()], 3, edges).unwrap(),
        collect_instances([b.clone(), a.clone()], 3, edges).unwrap()
    );
    let root = InstanceKey {
        definition: definition("growing"),
        types: vec!["u64".into()],
        dictionaries: vec![],
    };
    let stop = collect_instances([root], 8, |key| {
        let mut next = key.clone();
        next.types[0] = format!("List[{}]", key.types[0]);
        Ok(vec![next])
    })
    .unwrap_err();
    assert!(matches!(stop, InstanceStop::Resource { limit: 8, .. }));
}
#[test]
fn checked_instance_collection_follows_calls_without_body_rechecks() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request(
                "fn leaf() -> u64 { 42 }\nfn root() -> u64 { leaf() }\n",
                dir.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    let root = product
        .definitions
        .keys()
        .find(|p| p.0.name == "root")
        .unwrap();
    let root = hirsel_core_ir::InstanceKey {
        definition: root.clone(),
        types: vec![],
        dictionaries: vec![],
    };
    let instances = product.instances([root], 8).unwrap();
    assert_eq!(
        instances
            .iter()
            .map(|i| i.definition.0.name.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "root"]
    );
    assert_eq!(names(&product), ["leaf", "root"]);
}

#[test]
fn entry_waits_policy_is_an_explicit_cached_request_input() {
    let dir = tempfile::tempdir().unwrap();
    let source = "pub fn main() waits {}\n";
    let mut driver = Driver::default();
    let mut req = request(source, dir.path());
    req.entry = Some(DefPath::from_full("app@1/main/main").unwrap());
    let entry = driver.check(req, &mut Meter::new(false)).unwrap();
    // PROG-3 now refuses unit main. Entry still suppresses the unused-waits
    // refusal; checking this same body as an ordinary function must restore it.
    assert!(entry.diagnostics.iter().any(|d| d.rule == "PROG-3"));
    assert!(!entry.diagnostics.iter().any(|d| d.rule == "LANG-8"));
    let library = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(!library.accepted());
    assert_eq!(names(&library), ["main"]);
    assert!(library.diagnostics.iter().any(|d| d.rule == "LANG-8"));
}

#[test]
fn an_unused_image_declaration_section_is_not_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = image.unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    std::fs::write(store.path(image.directory.archive), b"damaged").unwrap();
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn scalar() -> u64 { 42 }\n", app.path());
    req.library = LibraryInput::Image(Arc::clone(&opened));
    assert!(
        Driver::default()
            .check(req, &mut Meter::new(false))
            .unwrap()
            .accepted()
    );
    let mut req = request("fn caller() -> u64 { lib.answer() }\n", app.path());
    req.library = LibraryInput::Image(opened);
    assert!(
        Driver::default()
            .check(req, &mut Meter::new(false))
            .err()
            .unwrap()
            .to_string()
            .contains("cache miss")
    );
}

#[cfg(unix)]
#[test]
fn directory_source_file_symlinks_are_checked_instead_of_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let package_dir = dir.path().join("package");
    std::fs::create_dir(&package_dir).unwrap();
    let source = dir.path().join("source.hirsel");
    std::fs::write(&source, "fn answer() -> u64 { 6 * 7 }\n").unwrap();
    std::os::unix::fs::symlink(source, package_dir.join("main.hirsel")).unwrap();
    let inputs = CorpusManifest {
        core: package("core"),
        packages: vec![PackageManifest::directory(&package_dir, package("app"), false).unwrap()],
    };
    let product = Driver::default()
        .check(
            CheckRequest {
                inputs,
                store: dir.path().join("store"),
                library: LibraryInput::Sources,
                entry: None,
            },
            &mut Meter::new(false),
        )
        .unwrap();
    assert_eq!(names(&product), ["answer"]);
    let body = product.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}

#[test]
fn installed_image_instances_are_demanded_without_source_rechecks() {
    let lib = tempfile::tempdir().unwrap();
    Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", lib.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn caller() -> u64 { lib.answer() }\n", app.path());
    req.library = LibraryInput::Image(Arc::clone(&image));
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    assert_eq!(image.decoded_bodies(), 0);
    let key = hirsel_core_ir::InstanceKey {
        definition: DefPath::from_full("app@1/main/caller").unwrap(),
        types: vec![],
        dictionaries: vec![],
    };
    let instances = product.instances([key], 8).unwrap();
    assert_eq!(instances.len(), 2);
    assert_eq!(image.decoded_bodies(), 1);
    assert_eq!(names(&product), ["caller"]);
}

#[test]
fn invalid_utf8_diagnostics_keep_raw_byte_spans() {
    let dir = tempfile::tempdir().unwrap();
    let req = request("", dir.path());
    std::fs::write(dir.path().join("main.hirsel"), [0xf0, 0x28, 0x8c, 0x28]).unwrap();
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let diagnostics = product.machine_diagnostics().unwrap();
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0].primary.module, "app@1/main");
    assert_eq!(
        diagnostics[0].primary.span,
        hirsel_core_syntax::Span { start: 0, end: 1 }
    );
}

#[test]
fn session_id_changes_reuse_and_rebind_cached_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn bad() -> u64 { let n = 7; 0 }\n";
    let mut driver = Driver::default();
    let old = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let earlier = dir.path().join("earlier.hirsel");
    std::fs::write(&earlier, "fn extra() -> u64 { 42 }\n").unwrap();
    let configured = || {
        let mut req = request(source, dir.path());
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file: earlier.clone(),
                modules: vec!["aaa".into()],
                signatures: false,
            });
        req
    };
    let edited = driver.check(configured(), &mut Meter::new(false)).unwrap();
    assert_eq!(names(&edited), ["extra"]);
    assert_ne!(
        old.diagnostics[0].primary.module,
        edited.diagnostics[0].primary.module
    );
    let disk = Driver::default()
        .check(configured(), &mut Meter::new(false))
        .unwrap();
    assert!(disk.rechecked.is_empty());
    for product in [&edited, &disk] {
        let diagnostics = product.machine_diagnostics().unwrap();
        let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
        assert_eq!(refusal.primary.module, "app@1/main");
        let span = refusal.primary.span;
        assert_eq!(&source[span.start as usize..span.end as usize], "7");
    }
}

#[test]
fn incomplete_source_is_not_an_emittable_instance() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request(
                "fn root() -> u64 { let f = fn() { 42u64 }; 42 }\n",
                dir.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.incomplete.is_empty());
    let key = hirsel_core_ir::InstanceKey {
        definition: DefPath::from_full("app@1/main/root").unwrap(),
        types: vec![],
        dictionaries: vec![],
    };
    assert!(matches!(
        product.instances([key], 8),
        Err(InstanceStop::Incomplete(_))
    ));
}

#[test]
fn reachable_build_ignores_unreached_bodies_and_declarations_after_disk_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn helper() -> u8 { 7u8 }\nfn answer() -> u8 { helper() }\nfn unused[T](take x: T) -> T { x }\ntype Broken = Missing\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        !product.accepted(),
        "whole-package check still rejects the input"
    );
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(reachable.accepted(), "{:?}", reachable.instances);
    assert_eq!(reachable.instances.as_ref().unwrap().len(), 2);
    assert_eq!(reachable.bodies.len(), 2);
    assert!(reachable.first_stops.is_empty());
    let cached = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(cached.rechecked.is_empty());
    assert!(!cached.accepted());
    assert!(cached.reachable(&entry, 64).unwrap().accepted());
}

#[test]
fn reachable_build_refuses_and_censuses_the_incomplete_callee() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u8 { unfinished() }\nfn unfinished() -> u8 { let _ = fn(x: u8) -> u8 { x }; 7u8 }\nfn unused[T](take x: T) -> T { x }\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(!reachable.accepted());
    let stopped = reachable.require().unwrap_err().to_string();
    assert!(stopped.contains("unfinished"), "{stopped}");
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/unfinished")
    );
    assert!(
        !reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/unused")
    );
    assert_eq!(reachable.census().values().sum::<usize>(), 1);
}

#[test]
fn reachable_build_keeps_refusals_in_named_signature_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let source = "type Broken = Missing\nfn inspect(x: Broken) -> u8 { 7u8 }\nfn answer() -> u8 { inspect(7u8) }\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(!reachable.accepted());
    assert!(!reachable.diagnostics.is_empty());
    assert!(
        reachable
            .declarations
            .contains(&DefPath::from_full("app@1/main/Broken").unwrap())
    );
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/Broken")
    );
}

#[test]
fn reachable_census_follows_names_beyond_an_incomplete_entry() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u8 { let _ = fn(x: u8) -> u8 { x }; unfinished() }\nfn unfinished() -> u8 { let _ = fn(x: u8) -> u8 { x }; 7u8 }\n";
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product
        .reachable(&DefPath::from_full("app@1/main/answer").unwrap(), 64)
        .unwrap();
    assert!(!reachable.accepted());
    assert_eq!(reachable.bodies.len(), 2);
    assert_eq!(reachable.first_stops.len(), 2);
    assert_eq!(reachable.census().values().sum::<usize>(), 2);
}

#[test]
fn reachable_census_retains_a_named_function_without_a_native_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u8 { external() }\n", dir.path());
    let library = dir.path().join("core.hirsel");
    std::fs::write(&library, "pub fn external() -> u8\n").unwrap();
    req.inputs.packages[0].dependencies.push(package("core"));
    req.inputs.packages.push(PackageManifest {
        package: package("core"),
        sources: vec![hirsel_core_resolve::ModuleSource {
            file: library,
            modules: vec!["core".into()],
            signatures: true,
        }],
        dependencies: vec![],
    });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let reachable = product
        .reachable(&DefPath::from_full("app@1/main/answer").unwrap(), 64)
        .unwrap();
    assert!(!reachable.accepted());
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "core@1/core/external"
                && s.construct == "native body is unavailable")
    );
    assert!(
        reachable
            .require()
            .unwrap_err()
            .to_string()
            .contains("external")
    );
}

#[test]
fn indirect_instances_follow_the_values_function_references() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(request(
        "fn leaf(x: u64) -> u64 { x + 1 }\nfn apply(cb: fn(u64) -> u64, x: u64) -> u64 { cb(x) }\nfn root() -> u64 { let cb: fn(u64) -> u64 = leaf; apply(cb, 41) }\n",
        dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted());
    let root = hirsel_core_ir::InstanceKey {
        definition: DefPath::from_full("app@1/main/root").unwrap(),
        types: vec![],
        dictionaries: vec![],
    };
    let instances = product.instances([root], 8).unwrap();
    assert_eq!(
        instances
            .iter()
            .map(|i| i.definition.0.name.as_str())
            .collect::<Vec<_>>(),
        ["apply", "leaf", "root"]
    );
}
