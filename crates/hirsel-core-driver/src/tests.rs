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
fn publishes_a_checked_body_record_that_runs_like_the_checked_body() {
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
    let index = check_index::ProgramIndex::open(&store).unwrap();
    assert_eq!(index.root, Some(product.root));
    let (_, record) = index.live(&d.definition).unwrap();
    let disk: DiskCheck = decode(&store.get(record).unwrap().unwrap()).unwrap();
    assert_eq!(
        eval::run(&disk.ir, FuncId(disk.function.unwrap()), &[], 1_000_000).outcome,
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
fn prog8_unrelated_parse_failure_does_not_suppress_other_results() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u64 { 42 }\nfn independently_bad() -> u64 { true }\n";
    let mut req = request(source, dir.path());
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
    let diagnostics = product.machine_diagnostics().unwrap();
    assert!(
        diagnostics.len() >= 2,
        "{}",
        serde_json::to_string(&diagnostics).unwrap()
    );
    assert!(diagnostics.iter().any(|d| d.rule == "LANG-11"
        && d.primary.module == "app@1/main"
        && &source[d.primary.span.start as usize..d.primary.span.end as usize] == "true"));
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
    // `outer` reads only `caller`'s signature, which a body name of `caller`
    // does not enter, wherever `caller` sits in the file.
    let a = "fn callee() -> u64 { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\nfn outer() -> u64 { caller() }\n";
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
fn prog1_prog6_transitive_type_reads_and_discovery_order_have_stable_results() {
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
    store.select(check_index::SELECTION, old).unwrap();
    let failure = store.archive(|writer| {
        use std::io::Write;
        writer.write_all(b"unfinished next result")?;
        Err(io::Error::other("injected publication failure"))
    });
    assert!(failure.is_err());
    assert_eq!(store.selection(check_index::SELECTION).unwrap(), Some(old));
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
    // The caller's key reads the installed signatures it names: an image
    // header edit changes it and an image body edit does not.
    let caller_signatures = |library: &str| {
        let lib = tempfile::tempdir().unwrap();
        let (_, image) = Driver::default()
            .produce_image(library_request(library, lib.path()), &mut Meter::new(false))
            .unwrap();
        let app = tempfile::tempdir().unwrap();
        let mut req = request(
            "fn caller(x: lib.Number) -> u64 { x + lib.answer() }\n",
            app.path(),
        );
        req.library = LibraryInput::Image(image.unwrap());
        let checked = Driver::default()
            .check(req, &mut Meter::new(false))
            .unwrap();
        checked.definitions.values().next().unwrap().key.signatures
    };
    let caller = checked.definitions.values().next().unwrap();
    assert_eq!(
        caller.key.signatures,
        caller_signatures("pub type Number = u64\npub fn answer() -> Number { 43 }\n")
    );
    assert_ne!(
        caller.key.signatures,
        caller_signatures("pub type Number = u64\npub fn answer() -> u64 { 42 }\n")
    );
    let answer = DefPath::from_full("core@1/lib/answer").unwrap();
    let body = checked.demand_body(&answer).unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 1));
    let sources = checked
        .native_sources(std::slice::from_ref(&answer))
        .unwrap();
    let source = &sources.items[&answer];
    assert_eq!(source.file_offset, 22);
    assert_eq!(
        sources.files[source.file as usize].text,
        "pub type Number = u64\npub fn answer() -> Number { 42 }\n"
    );
    assert_eq!(
        sources.files[source.file as usize].path,
        dir.path().join("main.hirsel").to_string_lossy()
    );
    assert_eq!(
        (opened.decoded_files(), opened.decoded_bodies()),
        (0, 1),
        "native source binding must not decode a published syntax tree"
    );
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
    let hash = image.directory().unwrap().definitions[&answer];
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
fn prog7_machine_refusal_spans_point_into_current_file_bytes() {
    let dir = tempfile::tempdir().unwrap();
    // The refused `n` is the found `u16` where the result's `u64` is expected (LANG-11):
    // the machine form carries both facts, found as declared and expected as required.
    let source =
        "// leading comment\nfn good() -> u64 { 42 }\nfn bad() -> u64 { let n: u16 = 7; n }\n";
    let result = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let diagnostics = result.machine_diagnostics().unwrap();
    let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
    assert_eq!(refusal.primary.module, "app@1/main");
    let span = refusal.primary.span;
    assert_eq!(span.start as usize, source.rfind('n').unwrap());
    assert_eq!(&source[span.start as usize..span.end as usize], "n");
    let json = serde_json::to_value(refusal).unwrap();
    assert_eq!(json["declared"], "u16");
    assert_eq!(json["required"], "u64");
    assert_eq!(json["causal"], serde_json::json!([]));
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
    std::fs::write(
        store.path(image.directory().unwrap().declarations),
        b"damaged",
    )
    .unwrap();
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
fn prog7_invalid_utf8_diagnostics_keep_raw_byte_spans() {
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
            request("fn root() -> u64 { let _ = b\"bytes\"; 42 }\n", dir.path()),
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

/// Bug: a resource refusal stored with the product, so a build under a larger instance limit
/// is still refused from the store (or a smaller limit is served an earlier acceptance).
/// The instance limit belongs to the build, not to the stored product: the same stored
/// product is refused at limit 1 (at the second instance's definition, with the amount
/// and the limit) and accepted at 64.
#[test]
fn instance_limit_refusal_is_per_build_and_never_served_from_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn helper() -> u8 { 7u8 }\nfn answer() -> u8 { helper() }\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let first = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(first.resources.is_empty());
    let tight = first.reachable(&entry, 1).unwrap();
    assert!(!tight.accepted());
    assert!(tight.incomplete.is_empty() && tight.first_stops.is_empty());
    let [resource] = tight.resources.as_slice() else {
        panic!("{:?}", tight.resources)
    };
    let at = resource.primary.span;
    assert_eq!(&source[at.start as usize..at.end as usize], "helper");
    assert!(
        resource
            .message
            .contains("needs instance 2 past the limit of 1")
    );
    let stored = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(stored.rechecked.is_empty());
    assert!(stored.resources.is_empty());
    assert!(stored.reachable(&entry, 64).unwrap().accepted());
    assert!(!stored.reachable(&entry, 1).unwrap().resources.is_empty());
}

#[test]
fn reachable_build_refuses_and_censuses_the_incomplete_callee() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u8 { unfinished() }\nfn unfinished() -> u8 { let _ = b\"bytes\"; 7u8 }\nfn unused[T](take x: T) -> T { x }\n";
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
    // The declaration's refusal is kept wherever the declaration sits in its file.
    for source in [
        "type Broken = Missing\nfn inspect(x: Broken) -> u8 { 7u8 }\nfn answer() -> u8 { inspect(7u8) }\n",
        "fn inspect(x: Broken) -> u8 { 7u8 }\nfn answer() -> u8 { inspect(7u8) }\ntype Broken = Missing\n",
    ] {
        reachable_build_keeps_the_declaration_refusal(source);
    }
}
fn reachable_build_keeps_the_declaration_refusal(source: &str) {
    let dir = tempfile::tempdir().unwrap();
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
    let source = "fn answer() -> u8 { let _ = b\"bytes\"; unfinished() }\nfn unfinished() -> u8 { let _ = b\"bytes\"; 7u8 }\n";
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

#[test]
fn generic_templates_survive_disk_reuse_and_materialize_the_call_closure() {
    use hirsel_core_ir::InstanceKey;
    let dir = tempfile::tempdir().unwrap();
    let source = "fn id[T](take x: T) -> T { x }\nfn forward[T](take x: T) -> T { id(take x) }\nfn discard[T](take x: T) {}\nfn byte() -> u8 { forward(7u8) }\nfn root() -> u64 { forward(72623859790382856u64) }\n";
    let fresh = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        fresh.accepted(),
        "{:?} {:?}",
        fresh.diagnostics,
        fresh.incomplete
    );
    let cached = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(cached.rechecked.is_empty());
    let root = DefPath::from_full("app@1/main/root").unwrap();
    let reached = cached.reachable(&root, 16).unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let keys = reached.instances.as_ref().unwrap();
    assert_eq!(keys.len(), 3);
    for name in ["id", "forward"] {
        let path = DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
        let key = InstanceKey {
            definition: path.clone(),
            types: vec!["u64".into()],
            dictionaries: vec![],
        };
        assert!(keys.contains(&key));
        let source = cached.demand(&path).unwrap();
        let template = source.check.template.as_ref().unwrap();
        assert!(!template.copies.is_empty());
        let ir = reached.materialized.get(&key).ir();
        let layouts = hirsel_core_layout::compute(&ir.types).unwrap();
        let object = hirsel_cranelift::core::emit_object_with_layouts(
            ir,
            &layouts,
            &hirsel_cranelift::core::Options {
                target: native::TARGET,
                optimize: false,
                debug_lines: false,
            },
            ContentHash::of_bytes(b"materialized test unit"),
        )
        .unwrap();
        assert!(!object.bytes.is_empty());
    }
    // T remains potentially owned in the once-checked body. Its placeholder
    // must retain cleanup even when no particular instantiation has been requested.
    let discard = cached
        .demand(&DefPath::from_full("app@1/main/discard").unwrap())
        .unwrap();
    assert!(
        discard.ir.functions[discard.check.function.unwrap().index()]
            .body
            .as_ref()
            .unwrap()
            .insts
            .iter()
            .any(|i| matches!(i, hirsel_core_ir::Inst::Drop { .. }))
    );
    let byte = InstanceKey {
        definition: DefPath::from_full("app@1/main/byte").unwrap(),
        types: vec![],
        dictionaries: vec![],
    };
    let roots = InstanceKey {
        definition: root,
        types: vec![],
        dictionaries: vec![],
    };
    assert_eq!(cached.instances([byte, roots], 16).unwrap().len(), 6);
}

#[test]
fn installed_generic_template_needs_no_source_or_body_recheck() {
    let lib = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub fn identity[T](take value: T) -> T { value }\n",
                lib.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(image.is_some());
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(lib.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn root() -> u64 { lib.identity(72623859790382856u64) }\n",
        app.path(),
    );
    req.library = LibraryInput::Image(Arc::clone(&image));
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    assert_eq!(image.decoded_bodies(), 0);
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/root").unwrap(), 16)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    assert_eq!(reached.instances.as_ref().unwrap().len(), 2);
    assert_eq!(image.decoded_bodies(), 1);
}

#[test]
fn closure_instances_include_calls_inside_the_generated_body() {
    let dir = tempfile::tempdir().unwrap();
    let product=Driver::default().check(request("fn leaf(x: u64) -> u64 {x + 1}\nfn root() -> u64 {let cb: fn(u64) -> u64 = fn(x: u64) {leaf(x)}; cb(41)}\n",dir.path()),&mut Meter::new(false)).unwrap();
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
        ["leaf", "root"]
    );
}

#[test]
fn scalar_constants_survive_installed_images_and_invalidate_body_reuse() {
    let lib = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub const ANSWER: u64 = BASE + 2\nconst BASE: u64 = 40\n",
                lib.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(image.is_some());
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(lib.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u64 { lib.ANSWER }\n", app.path());
    req.library = LibraryInput::Image(image);
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let body = product.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[], 1000).outcome,
        eval::Outcome::Returned(vec![42])
    );

    let local = tempfile::tempdir().unwrap();
    let before = "const N: u8 = 7\nfn answer() -> u8 { N }\n";
    Driver::default()
        .check(request(before, local.path()), &mut Meter::new(false))
        .unwrap();
    let after = Driver::default()
        .check(
            request(&before.replace("= 7", "= 9"), local.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert_eq!(names(&after), ["answer"]);
    let body = after.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[], 1000).outcome,
        eval::Outcome::Returned(vec![9])
    );
}

#[test]
fn unused_failed_constants_block_entry_build_acceptance_after_disk_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let source = "const OVERFLOW: u8 = 200 + 100\nfn answer() -> u8 { 42 }\n";
    let root = DefPath::from_full("app@1/main/answer").unwrap();
    for _ in 0..2 {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let reached = product.reachable(&root, 16).unwrap();
        assert!(!reached.accepted());
        assert!(reached.diagnostics.iter().any(|d| d.rule == "LANG-20"));
    }
}

#[test]
fn lowered_runtime_operation_needs_no_outside_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn answer() -> u64 { task.check_cancel(); 42u64 }",
        dir.path(),
    );
    let core = dir.path().join("task.hirsel");
    std::fs::write(&core, "pub fn check_cancel()\n").unwrap();
    req.inputs.packages[0].dependencies.push(package("core"));
    req.inputs.packages.push(PackageManifest {
        package: package("core"),
        sources: vec![hirsel_core_resolve::ModuleSource {
            file: core,
            modules: vec!["task".into()],
            signatures: true,
        }],
        dependencies: vec![],
    });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(reachable.accepted(), "{:?}", reachable.instances);
    let instances = reachable.instances.unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].definition, entry);
    let definition = &product.definitions[&entry];
    assert_eq!(
        eval::run(
            &definition.ir,
            definition.check.function.unwrap(),
            &[],
            1024
        )
        .outcome,
        eval::Outcome::Returned(vec![42])
    );
}

#[test]
fn nested_cost_views_exclude_bookkeeping_without_double_counting() {
    use hirsel_core_front::cost::{Scope, finish, start};
    let clock = std::cell::Cell::new(0);
    start(move || {
        clock.set(clock.get() + 10);
        Ok(clock.get())
    })
    .unwrap();
    {
        let _outer = Scope::enter("phase", || "body".into());
        let _construct = Scope::enter("construct", || "call".into());
        let _inner = Scope::enter("phase", || "layout".into());
    }
    let profile = finish();
    let row = |name: &str| profile.rows.iter().find(|r| r.name == name).unwrap();
    // Six event boundaries, each with ten instructions of bookkeeping and ten
    // of compiler work. Only same-view descendants reduce self cost.
    assert_eq!((row("body").inclusive, row("body").exclusive), (50, 40));
    assert_eq!((row("layout").inclusive, row("layout").exclusive), (10, 10));
    assert_eq!((row("call").inclusive, row("call").exclusive), (30, 30));
    assert_eq!(profile.excluded_instrumentation, 60);
    assert_eq!(row("body").occurrences, 1);
    assert!(profile.counter_refusal.is_none());
}

#[test]
fn aggregate_constants_in_an_image_need_no_source_or_body_decode() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default().produce_image(library_request("pub type Point { x: u8, y: u64 }\npub const ORIGIN: Point = Point(x: 2, y: 40)\npub const TABLE: Inline[(u8,u64),2] = [(1,7),(2,11)]\n", dir.path()), &mut Meter::new(false)).unwrap();
    assert!(image.is_some());
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(dir.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn answer(i:u64) -> u64 { let (_, value) = lib.TABLE[i]; value + lib.ORIGIN.y }\n",
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
    let path = DefPath::from_full("app@1/main/answer").unwrap();
    let body = checked.demand_body(&path).unwrap();
    assert_eq!(
        eval::run(&body.ir, body.check.function.unwrap(), &[1], 1_000_000).outcome,
        eval::Outcome::Returned(vec![51])
    );
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
}

#[test]
fn indexed_combined_libraries_keep_existing_module_extensions() {
    let dir = tempfile::tempdir().unwrap();
    fn input(
        dir: &Path,
        name: &str,
        module: &str,
        body: &str,
        deps: Vec<PackageId>,
    ) -> PackageManifest {
        let file = dir.join(format!("{name}-{module}.hirsel"));
        std::fs::write(&file, body).unwrap();
        PackageManifest {
            package: package(name),
            dependencies: deps,
            sources: vec![hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![module.into()],
                signatures: name != "app",
            }],
        }
    }
    let core = input(
        dir.path(),
        "core",
        "ops",
        "pub fn base() -> u64 { 7u64 }",
        vec![],
    );
    let std = input(
        dir.path(),
        "std",
        "ops",
        "pub fn extra() -> u64 { 11u64 }",
        vec![package("core")],
    );
    let (_, image) = Driver::default()
        .produce_image(
            CheckRequest {
                inputs: CorpusManifest {
                    core: package("core"),
                    packages: vec![core, std],
                },
                store: dir.path().join("image"),
                entry: None,
                library: LibraryInput::Sources,
            },
            &mut Meter::new(false),
        )
        .unwrap();
    let opened = CoreImage::open(&image.unwrap().store_root())
        .unwrap()
        .unwrap();
    let mut app = input(
        dir.path(),
        "app",
        "ops",
        "pub fn other() -> u64 { 13u64 }",
        vec![package("core"), package("std")],
    );
    let mut main = input(
        dir.path(),
        "app",
        "main",
        "pub fn f() -> u64 { ops.base() + ops.extra() + ops.other() }",
        vec![],
    );
    app.sources.append(&mut main.sources);
    let product = Driver::default()
        .check(
            CheckRequest {
                inputs: CorpusManifest {
                    core: package("core"),
                    packages: vec![app],
                },
                store: dir.path().join("app"),
                entry: None,
                library: LibraryInput::Image(opened),
            },
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(
        product.accepted(),
        "{}",
        serde_json::to_string(&(
            product.machine_diagnostics().unwrap(),
            product.machine_incomplete().unwrap()
        ))
        .unwrap()
    );
    let reached = product
        .instances(
            [hirsel_core_ir::InstanceKey {
                definition: DefPath::from_full("app@1/main/f").unwrap(),
                types: vec![],
                dictionaries: vec![],
            }],
            16,
        )
        .unwrap();
    let paths: std::collections::BTreeSet<_> =
        reached.iter().map(|k| k.definition.to_string()).collect();
    assert_eq!(
        paths,
        std::collections::BTreeSet::from([
            "app@1/main/f".into(),
            "app@1/ops/other".into(),
            "core@1/ops/base".into(),
            "std@1/ops/extra".into()
        ])
    );
}

#[test]
fn copied_indexed_image_reads_its_live_store() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 7u64 }", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let original = image.unwrap();
    let copied = dir.path().join("copied");
    std::fs::create_dir(&copied).unwrap();
    for entry in std::fs::read_dir(original.store_root()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), copied.join(entry.file_name())).unwrap();
        }
    }
    let reopened = CoreImage::open(&copied).unwrap().unwrap();
    std::fs::remove_dir_all(original.store_root()).unwrap();
    assert!(
        reopened
            .resolve_name(&DefPath::from_full("core@1/lib/answer").unwrap())
            .unwrap()
            .is_some()
    );
    assert!(
        reopened
            .demand_declaration(&DefPath::from_full("core@1/lib/answer").unwrap())
            .unwrap()
    );
}

#[test]
fn parser_nesting_exhaustion_keeps_its_file_site_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let limit = hirsel_core_syntax::PARSER_NESTING_LIMIT;
    let source = format!(
        "// header\ntype T = {}u64{}",
        "List[".repeat(limit),
        "]".repeat(limit)
    );
    let at = source.find("u64").unwrap() as u32;
    let product = Driver::default()
        .check(request(&source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    assert!(product.diagnostics.is_empty());
    assert!(product.incomplete.is_empty());
    let resource = product
        .resources
        .iter()
        .find(|r| r.message.starts_with("parser resource refusal:"))
        .unwrap();
    assert_eq!(resource.primary.item, u32::MAX);
    assert_eq!(
        resource.primary.span,
        hirsel_core_syntax::Span {
            start: at,
            end: at + 3
        }
    );
    assert!(
        resource
            .message
            .contains(&format!("nesting limit of {limit}"))
    );
}

#[test]
fn store_paths_keep_two_lowercase_hex_digits_per_digest_byte() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::new(dir.path()).unwrap();
    let pattern = [0, 1, 16, 127, 128, 254, 255, 170];
    let key = ContentHash(std::array::from_fn(|i| pattern[i % pattern.len()]));
    assert_eq!(
        store.path(key),
        dir.path().join("0001107f80feffaa".repeat(4))
    );
}

#[test]
fn prog4_source_std_excludes_contract_directory() {
    let dir = tempfile::tempdir().unwrap();
    let libraries = dir.path().join("libraries");
    let core = libraries.join("core");
    let std = libraries.join("std");
    let app = dir.path().join("app");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::create_dir_all(std.join("tests")).unwrap();
    std::fs::create_dir_all(app.join("tests")).unwrap();
    std::fs::write(
        app.join("tests/local.hirsel"),
        "@test\nfn local() -> Result[(), ()] { Ok(()) }\n",
    )
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for source in PackageProduct::bundled_manifest(&root.join("core/core")).sources {
        std::fs::copy(&source.file, core.join(source.file.file_name().unwrap())).unwrap();
    }
    let library = std.join("probe.hirsel");
    std::fs::write(&library, "pub fn answer() -> u64 { 7 }\n").unwrap();
    std::fs::write(
        std.join("tests/private.hirsel"),
        "// test-of: std.probe\nfn deliberately broken(\n",
    )
    .unwrap();
    std::fs::write(
        std.join("tests/public.hirsel"),
        "fn ordinary() -> u64 { 9 }\n",
    )
    .unwrap();
    std::fs::write(
        std.join("tests/later.hirsel"),
        "// ordinary fixture\n// test-of: std.probe\nfn later() -> u64 { 11 }\n",
    )
    .unwrap();
    std::fs::write(app.join("main.hirsel"), "@test\nfn law() -> Result[(), ()] { if std.probe.answer() == 7 { Ok(()) } else { Error(()) } }\n").unwrap();
    let store = dir.path().join("store");
    for (expected_tests, mut request) in [
        CheckRequest::directory(&app, &libraries, &store).unwrap(),
        CheckRequest::libraries(&libraries, &store).unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        request.entry = None;
        let modules: Vec<_> = request.inputs.packages[1]
            .sources
            .iter()
            .map(|s| s.modules.join("/"))
            .collect();
        assert_eq!(modules, ["probe"]);
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        assert_eq!(
            product.tests().count(),
            2 * usize::from(expected_tests == 0)
        );
    }
    // A marker on a library source must never hide a real source refusal.
    std::fs::write(
        &library,
        "// test-of: std.probe\npub fn answer() -> u64 { true }\n",
    )
    .unwrap();
    let mut request = CheckRequest::directory(&app, &libraries, &store).unwrap();
    request.entry = None;
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    assert!(
        product.diagnostics.iter().any(|r| r.rule == "LANG-11"
            && product.index.module_path(r.primary.module).modules == ["probe"])
    );
}

#[test]
fn canonical_driver_encoding_visits_the_value_once() {
    struct Counted<'a>(&'a std::cell::Cell<usize>, &'a DefPath);
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            self.1.serialize(serializer)
        }
    }
    let path = DefPath(hirsel_core_ids::ItemPath {
        package: hirsel_core_ids::PackageId {
            name: "package".repeat(20),
            version: "1".into(),
        },
        modules: vec!["nested".repeat(20), "é\0".into()],
        name: "value".repeat(20),
    });
    let visits = std::cell::Cell::new(0);
    let encoded = encode(&Counted(&visits, &path)).unwrap();
    assert_eq!(encoded, bincode::serialize(&path).unwrap());
    assert_eq!(decode::<DefPath>(&encoded), Some(path));
    assert_eq!(
        visits.get(),
        1,
        "canonical encoding made a sizing traversal"
    );
}

/// Sources of a small program over four modules: a shared helper, a mutually
/// recursive pair, the edited module and an independent reader of the helper.
fn program_sources(kind: &str) -> [(&'static str, String); 4] {
    let helper = "pub fn config(x: u64) -> u64 { x }\npub fn checksum(x: u64) -> u64 { x + 1 }\npub type Settings { n: u64 }\n";
    let shapes = "pub type Even = Zero | Next(Box[Odd])\npub type Odd = One(Box[Even])\npub fn depth(e: Even) -> u64 { 0 }\npub fn top(o: Odd) -> u64 { 1 }\n";
    let main = "pub fn dispatch(n: u64) -> u64 {\n    let total = helper.config(n)\n    total\n}\npub fn main() -> u64 {\n    dispatch(helper.config(3))\n}\npub fn uses_shapes(e: shapes.Even) -> u64 { shapes.depth(e) }\n";
    let other = "pub fn other(s: helper.Settings) -> u64 { helper.config(s.n) }\npub fn more() -> u64 { helper.config(2) }\n";
    let (mut h, s, mut m, mut o) = (
        helper.to_string(),
        shapes.to_string(),
        main.to_string(),
        other.to_string(),
    );
    let d = m.find("pub fn dispatch").unwrap();
    let e = m.find("pub fn main").unwrap();
    let u = m.find("pub fn uses_shapes").unwrap();
    match kind {
        "base" => {}
        "body" => m = m.replace("    total\n", "    let _ = 0u8\n    total\n"),
        "signature" => {
            h = h.replace("config(x: u64)", "config(x: u64, _extra: u64)");
            for t in [&mut m, &mut o] {
                *t = t
                    .replace("helper.config(n)", "helper.config(n, 0)")
                    .replace("helper.config(3)", "helper.config(3, 0)")
                    .replace("helper.config(s.n)", "helper.config(s.n, 0)")
                    .replace("helper.config(2)", "helper.config(2, 0)");
            }
        }
        "add" => m.push_str("pub fn extra_item() -> u32 {\n    7\n}\n"),
        "remove" => m = format!("{}{}", &m[..e], &m[u..]),
        "import" => m = m.replace("    total\n", "    total + helper.checksum(n)\n"),
        "rename" => m = m.replace("pub fn main(", "pub fn main_renamed("),
        "reorder" => m = format!("{}{}{}", &m[e..u], &m[d..e], &m[u..]),
        "comment" => m = format!("// a comment line above dispatch\n{m}"),
        "whitespace" => m = format!("\n\n{m}"),
        _ => unreachable!("{kind}"),
    }
    [("helper", h), ("shapes", s), ("main", m), ("other", o)]
}
fn program_request(kind: &str, dir: &Path, store: &Path) -> CheckRequest {
    let mut req = request("", dir);
    req.store = store.to_owned();
    req.inputs.packages[0].sources.clear();
    for (name, text) in program_sources(kind) {
        let file = dir.join(format!("{name}.hirsel"));
        std::fs::write(&file, text).unwrap();
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![name.into()],
                signatures: false,
            });
    }
    req
}
fn store_files(store: &Path) -> std::collections::BTreeSet<std::ffi::OsString> {
    std::fs::read_dir(store)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

/// §2d edit-kind gate. Law: an incremental build publishes the live rows and
/// file records of a fresh build of the same sources, and a tombstoned path
/// never resolves by path, by name or by anchor. Oracle: the fresh build.
#[test]
fn edit_kinds_publish_the_fresh_build_and_tombstones_never_resolve() {
    let mut report = vec![];
    for kind in [
        "body",
        "signature",
        "add",
        "remove",
        "import",
        "rename",
        "reorder",
        "comment",
        "whitespace",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let incremental = dir.path().join("incremental");
        Driver::default()
            .check(
                program_request("base", dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let before = store_files(&incremental);
        let built = Driver::default()
            .check(
                program_request(kind, dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let written = store_files(&incremental).difference(&before).count();
        let fresh = Driver::default()
            .check(
                program_request(kind, dir.path(), &dir.path().join("fresh")),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{kind}"
        );
        let open = |store: &Path| {
            let store = Store::new(store).unwrap();
            let index = check_index::ProgramIndex::open(&store).unwrap();
            (store, index)
        };
        let (store, index) = open(&incremental);
        let (fresh_store, fresh_index) = open(&dir.path().join("fresh"));
        let live = |index: &check_index::ProgramIndex| -> BTreeMap<DefPath, check_index::Row> {
            index
                .rows()
                .filter(|r| matches!(r.1, check_index::Row::Live { .. }))
                .map(|(p, r)| (p.clone(), *r))
                .collect()
        };
        assert_eq!(live(&index), live(&fresh_index), "{kind}: live rows");
        assert_eq!(fresh_index.rows().count(), live(&fresh_index).len());
        let modules: Vec<_> = program_sources(kind)
            .iter()
            .map(|(m, _)| ("app".to_string(), "1".to_string(), vec![m.to_string()]))
            .collect();
        for module in &modules {
            assert_eq!(
                index.file(&store, module).unwrap(),
                fresh_index.file(&fresh_store, module).unwrap(),
                "{kind}: file record {module:?}"
            );
        }
        let tombstones: Vec<DefPath> = index
            .rows()
            .filter(|r| matches!(r.1, check_index::Row::Removed { .. }))
            .map(|r| r.0.clone())
            .collect();
        assert_eq!(
            !tombstones.is_empty(),
            matches!(kind, "remove" | "rename"),
            "{kind}: {tombstones:?}"
        );
        for path in &tombstones {
            assert!(index.live(path).is_none());
            assert!(!built.definitions.contains_key(path));
            let module = check_index::module_key(path);
            assert_eq!(index.by_name(&store, &module, &path.0.name).unwrap(), None);
            for ordinal in 0..8 {
                let anchored = index.by_anchor(&store, &module, ordinal).unwrap();
                assert!(anchored.is_none_or(|(p, _)| p != *path), "{kind}: {path:?}");
            }
        }
        if kind == "body" {
            // One body record, one file record, the changed leaf and the root;
            // with the selection, one instance record and one object: at most 8.
            assert!(written + 3 <= 8, "body edit wrote {written} records");
        }
        report.push(format!(
            "{kind}: rechecked {} {:?}, records written {written}, tombstones {}",
            built.rechecked.len(),
            names(&built),
            tombstones.len()
        ));
    }
    eprintln!("{}", report.join("\n"));
}

/// An envelope with `payload` and a correct length, but the trailer (the
/// address) of `old`: only the digest tells it from the record it replaces.
fn under_old_address(old: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = old[..12].to_vec();
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&old[old.len() - 32..]);
    bytes
}

/// §2g record cases. Law: a damaged record, or a record of another toolchain,
/// is a miss and a rebuild whose publication equals a fresh build's; never a
/// different product. Oracle: a fresh build of the same sources in a new store.
#[test]
fn damaged_and_foreign_records_rebuild_the_fresh_publication() {
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = Driver::default()
        .check(
            program_request("base", fresh_dir.path(), &fresh_dir.path().join("store")),
            &mut Meter::new(false),
        )
        .unwrap();
    let dispatch = DefPath::from_full("app@1/main/dispatch").unwrap();
    let main = DefPath::from_full("app@1/main/main").unwrap();
    for case in [
        "torn root",
        "torn body",
        "truncated leaf, valid envelope",
        "swapped body records",
        "corrupted body payload, valid envelope",
        "another toolchain",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store");
        if case == "another toolchain" {
            reuse::TOOLCHAIN.set(Some(ContentHash::of_bytes(b"another compiler build")));
        }
        let base = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        reuse::TOOLCHAIN.set(None);
        let store = Store::new(&path).unwrap();
        let index = check_index::ProgramIndex::open(&store).unwrap();
        let body = |p: &DefPath| store.path(index.live(p).unwrap().1);
        let mut leaf_rows = vec![];
        let expect: Vec<&str> = match case {
            "torn root" => {
                let bytes = std::fs::read(store.path(base.root)).unwrap();
                std::fs::write(store.path(base.root), &bytes[..bytes.len() / 2]).unwrap();
                names(&fresh)
            }
            "torn body" => {
                let bytes = std::fs::read(body(&dispatch)).unwrap();
                std::fs::write(body(&dispatch), &bytes[..bytes.len() / 2]).unwrap();
                vec!["dispatch"]
            }
            "truncated leaf, valid envelope" => {
                // The leaf holding dispatch's row, cut to fewer rows: a valid
                // envelope whose payload decodes, under the old address.
                let leaf = std::fs::read_dir(&path)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|f| {
                        let Some(bytes) = std::fs::read(f).ok().filter(|b| b.len() > 52) else {
                            return false;
                        };
                        let payload = &bytes[12..bytes.len() - 40];
                        // Node::Leaf is the second variant of the node encoding.
                        payload[..4] == [1, 0, 0, 0]
                            && decode::<Vec<(DefPath, check_index::Row)>>(&payload[4..])
                                .is_some_and(|rows| rows.iter().any(|r| r.0 == dispatch))
                    })
                    .unwrap();
                let bytes = std::fs::read(&leaf).unwrap();
                let payload = &bytes[12..bytes.len() - 40];
                let mut rows: Vec<(DefPath, check_index::Row)> = decode(&payload[4..]).unwrap();
                leaf_rows = rows.iter().map(|r| r.0.0.name.clone()).collect();
                leaf_rows.sort_unstable();
                rows.retain(|r| r.0 != dispatch);
                let mut cut = payload[..4].to_vec();
                cut.extend(encode(&rows).unwrap());
                std::fs::write(&leaf, under_old_address(&bytes, &cut)).unwrap();
                // Every row below a damaged node is a miss.
                leaf_rows.iter().map(String::as_str).collect()
            }
            "swapped body records" => {
                std::fs::copy(body(&main), body(&dispatch)).unwrap();
                vec!["dispatch"]
            }
            "corrupted body payload, valid envelope" => {
                let bytes = std::fs::read(body(&dispatch)).unwrap();
                let mut payload = bytes[12..bytes.len() - 40].to_vec();
                let last = payload.len() - 1;
                payload[last] ^= 1;
                std::fs::write(body(&dispatch), under_old_address(&bytes, &payload)).unwrap();
                vec!["dispatch"]
            }
            "another toolchain" => names(&fresh),
            _ => unreachable!(),
        };
        let rebuilt = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(names(&rebuilt), expect, "{case}");
        assert_eq!(rebuilt.root, fresh.root, "{case}");
        let again = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            again.rechecked.is_empty(),
            "{case}: the rebuild repaired the store"
        );
    }
}

/// FORM #109: rows are reused across instance bodies by name, which is sound only if
/// one name is one type. Before one canonical encoding a substituted `String` field
/// was `Inline(String)` in one body and `Seq(u8)` in another, so a holder's layout
/// came from whichever body published the name first. Every monomorphic definition
/// of LangArena, std and the Core corpus (M1 programs, front cases) is a root.
#[test]
fn every_reached_shape_name_has_one_closure() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let files = |dir: &Path, tests: bool| -> Vec<hirsel_core_resolve::ModuleSource> {
        let mut paths = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(d) = pending.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    pending.push(p);
                } else if p.extension().is_some_and(|x| x == "hirsel") {
                    paths.push(p);
                }
            }
        }
        paths.sort();
        paths
            .into_iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(dir).unwrap().with_extension("");
                let modules: Vec<String> = rel.iter().map(|m| m.to_string_lossy().into()).collect();
                (tests || modules[0] != "tests").then_some(hirsel_core_resolve::ModuleSource {
                    file: p,
                    modules,
                    signatures: dir.ends_with("core"),
                })
            })
            .collect()
    };
    let core = PackageManifest {
        package: package("core"),
        sources: files(&root.join("core/core"), false),
        dependencies: vec![],
    };
    let std = PackageManifest {
        package: package("std"),
        sources: files(&root.join("core/std"), false),
        dependencies: vec![package("core")],
    };
    let mut m1: CorpusManifest = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m1/manifest.json")).unwrap(),
    )
    .unwrap();
    for s in m1.packages.iter_mut().flat_map(|p| &mut p.sources) {
        s.file = root.join(&s.file);
    }
    let langarena = CorpusManifest {
        core: package("core"),
        packages: vec![
            core,
            std,
            PackageManifest {
                package: package("langarena"),
                sources: files(&root.join("core/langarena"), true),
                dependencies: vec![package("core"), package("std")],
            },
        ],
    };
    let dir = tempfile::tempdir().unwrap();
    let mut requests: Vec<(String, CheckRequest)> = [("langarena", langarena), ("m1", m1)]
        .into_iter()
        .map(|(name, inputs)| {
            let store = dir.path().join(name);
            (
                name.to_string(),
                CheckRequest {
                    inputs,
                    store,
                    library: LibraryInput::Sources,
                    entry: None,
                },
            )
        })
        .collect();
    for (i, case) in files(&root.join("core/front/cases"), true)
        .into_iter()
        .enumerate()
    {
        let case_dir = dir.path().join(format!("case{i}"));
        std::fs::create_dir_all(&case_dir).unwrap();
        let source = std::fs::read_to_string(&case.file).unwrap();
        requests.push((case.file.display().to_string(), request(&source, &case_dir)));
    }
    let mut built = 0;
    for (name, request) in requests {
        // Names are one type within a product; separate cases reuse `app/main/P`.
        let mut closures = ShapeClosures::default();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let roots: Vec<_> = product
            .definitions
            .keys()
            .filter(|d| {
                product.demand_body(d).is_ok_and(|p| {
                    p.check.refusals.is_empty()
                        && p.incomplete.is_empty()
                        && p.check.function.is_some()
                        && p.check.template.is_none()
                })
            })
            .map(|d| hirsel_core_ir::InstanceKey {
                definition: d.clone(),
                types: vec![],
                dictionaries: vec![],
            })
            .collect();
        for root in roots {
            // A root whose reach includes an unfinished body is not a program.
            match product.shape_closures([root], 1 << 20, &mut closures) {
                Ok(()) => built += 1,
                Err(InstanceStop::Incomplete(_) | InstanceStop::OutsideReached(_)) => {}
                Err(stop) => panic!("{name}: {stop:?}"),
            }
        }
        assert!(
            closures.conflicts.is_empty(),
            "{name}: {:?}",
            closures.conflicts
        );
    }
    eprintln!("roots built {built}");
}

fn modules_request(files: &[(&str, &str)], dir: &Path, store: &Path) -> CheckRequest {
    let mut req = request("", dir);
    req.store = store.to_owned();
    req.inputs.packages[0].sources.clear();
    for (name, text) in files {
        let file = dir.join(format!("{name}.hirsel"));
        std::fs::write(&file, text).unwrap();
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![name.to_string()],
                signatures: false,
            });
    }
    req
}

/// Attack 960's resolution counterexamples 1 and 6 as edit-kind cases. Law:
/// after an edit, an incremental build reports the diagnostics, holds the
/// products and publishes the root of a fresh build of the same sources.
/// Oracle: the fresh build. Red before an unresolved body stopped recording
/// the resolver's refusals: its record kept the foreign type's old span.
#[test]
fn resolution_edges_rebuild_like_fresh() {
    let cases: [(&str, &[(&str, &str)], &[(&str, &str)]); 2] = [
        (
            "a variant added elsewhere takes over a record construction",
            &[(
                "a",
                "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\nconst K: R = R(x: 1u64)\n",
            )],
            &[
                (
                    "a",
                    "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\nconst K: R = R(x: 1u64)\n",
                ),
                ("b", "pub type V = R(u64)\n"),
            ],
        ),
        (
            "whitespace moves a foreign private type a refusal names",
            &[
                ("a", "fn f(x: b.Hidden) -> u64 { 0 }\n"),
                ("b", "type Hidden {}\n"),
            ],
            &[
                ("a", "fn f(x: b.Hidden) -> u64 { 0 }\n"),
                ("b", "type     Hidden {}\n"),
            ],
        ),
    ];
    for (case, base, edit) in cases {
        let dir = tempfile::tempdir().unwrap();
        let incremental = dir.path().join("incremental");
        Driver::default()
            .check(
                modules_request(base, dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let built = Driver::default()
            .check(
                modules_request(edit, dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let fresh = Driver::default()
            .check(
                modules_request(edit, dir.path(), &dir.path().join("fresh")),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{case}: diagnostics"
        );
        let records = |p: &CheckProduct| -> BTreeMap<DefPath, ContentHash> {
            p.definitions
                .iter()
                .map(|(k, d)| (k.clone(), d.record))
                .collect()
        };
        assert_eq!(records(&built), records(&fresh), "{case}: products");
        assert_eq!(built.root, fresh.root, "{case}: publication");
    }
}
