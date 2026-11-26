fn clone_request(source: &str, dir: &Path) -> CheckRequest {
    core_request("pub fn main() -> Result[(), Exit] { let p = probe.Probe(value: \"kept\"); let q: probe.Probe[String] = clone(p); if q.value == \"kept\" { Ok(()) } else { Error(Exit(status: 7)) } }", &[("probe", source)], dir)
}
/// An app over the real core with `probes` added to it as core modules.
fn core_request(main: &str, probes: &[(&str, &str)], dir: &Path) -> CheckRequest {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(main, dir);
    let mut core = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    for (name, source) in probes {
        let file = dir.join(format!("{name}.hirsel"));
        std::fs::write(&file, source).unwrap();
        core.sources.push(hirsel_core_resolve::ModuleSource {
                    foreign: None,
                    captured: None, file, modules: vec![name.to_string()], signatures: true });
    }
    request.inputs.packages[1] = core;
    request
}
const CLONE_PROBE: &str = "pub type Probe[T] { value: T }\nclone[T](p: Probe[T]) { let copied: T = clone(p.value); Probe(value: take copied) }\npub fn untouched() -> u64 { 42u64 }\n";

#[test]
fn clone_body_keys_preserve_type_identity_across_edits_and_reordering() {
    let dir = tempfile::tempdir().unwrap();
    let path = DefPath::from_full("core@1/probe/Probe.$clone").unwrap();
    let mut driver = Driver::default();
    let first = driver.check(clone_request(CLONE_PROBE, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(first.accepted(), "{:?} {:?}", first.diagnostics, first.incomplete);
    assert!(first.tests().next().is_none());
    assert!(first.headers.get(&path).unwrap().has_body, "a published clone is a demanded body");
    let changed = CLONE_PROBE.replace("let copied:", "let _ = 0u64; let copied:");
    let edited = driver.check(clone_request(&changed, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(edited.accepted(), "{:?} {:?}", edited.diagnostics, edited.incomplete);
    assert_eq!(edited.rechecked, [path.clone()]);
    let lines: Vec<_> = changed.lines().collect();
    let reordered = format!("{}\n{}\n{}\n", lines[2], lines[0], lines[1]);
    let reordered = Driver::default().check(clone_request(&reordered, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(reordered.accepted(), "{:?} {:?}", reordered.diagnostics, reordered.incomplete);
    assert!(reordered.rechecked.is_empty(), "{:?}", reordered.rechecked);
    assert_eq!(edited.definitions[&path].key, reordered.definitions[&path].key);
    assert_eq!(reordered.headers.get(&path).unwrap().anchor.2, 2);
}

fn clone_product(backend: Backend) {
    let lib = tempfile::tempdir().unwrap();
    let mut request = clone_request(CLONE_PROBE, lib.path());
    request.inputs.packages.remove(0);
    request.entry = None;
    let (product, image) = Driver::default().produce_image(request, &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let image = CoreImage::open(&image.unwrap().store_root()).unwrap().unwrap();
    let path = DefPath::from_full("core@1/probe/Probe.$clone").unwrap();
    assert!(image.has_body(&path).unwrap());
    std::fs::remove_file(lib.path().join("probe.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut request = clone_request(CLONE_PROBE, app.path());
    request.inputs.packages.pop();
    request.library = LibraryInput::Image(image);
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let reached = product.reachable(&entry, 64).unwrap();
    assert!(reached.accepted(), "{}", serde_json::to_string(&reached.first_stops).unwrap());
    assert!(reached.instances.as_ref().unwrap().iter().any(|i| i.definition == path && !i.closures.is_empty()));
    assert!(product.tests().next().is_none());
    let built = product.build(&entry, &native_options(backend, app.path())).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_product_body_is_reached_cranelift() { clone_product(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_product_body_is_reached_llvm() { clone_product(Backend::Llvm); }

/// Core module `held` declares `P`; `hook_a` and `hook_b` each hold an unrelated
/// function, and `P`'s clone hook, which adds one to the copy, is appended to
/// the module `hook` names. The app's `copies` clones a `P` holding 7.
fn hook_request(hook: Option<&str>, dir: &Path) -> CheckRequest {
    let held = "pub type P { value: u64 }\npub fn make(v: u64) -> P { P(value: v) }\npub fn value(p: P) -> u64 { p.value }\n";
    let with = |name: &str, other: &str| {
        let hooked = "clone(p: held.P) { held.make(held.value(p) + 1u64) }\n";
        format!("{other}{}", if hook == Some(name) { hooked } else { "" })
    };
    let (a, b) = (with("hook_a", "pub fn other() -> u64 { 0u64 }\n"), with("hook_b", "pub fn other() -> u64 { 1u64 }\n"));
    core_request("fn copies() -> u64 { let p = held.make(7u64); let copied: held.P = clone(p); held.value(copied) * 100u64 + held.value(p) }\npub fn main() -> Result[(), Exit] { let v = copies(); if v == 807u64 { Error(Exit(status: 8)) } else if v == 707u64 { Error(Exit(status: 7)) } else { Error(Exit(status: 9)) } }", &[("held", held), ("hook_a", &a), ("hook_b", &b)], dir)
}
/// A type's clone hook selection, absence included, keys every body that
/// clones the type. Law: after the hook moves to another module, appears or
/// goes, an incremental build rechecks the reader and its program runs like a
/// fresh build's. Oracle: the source, exit 8 with the hook (the copy holds 8)
/// and 7 without (a structural copy). Red before a type's header row read its
/// hook: a moved or removed hook kept the reader's old call to the old hook
/// path, which stopped the build, and an added one kept the structural copy.
fn hook_selection(backend: Backend) {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let reader = DefPath::from_full("app@1/main/copies").unwrap();
    let run = |product: &CheckProduct, dir: &Path| -> Result<Option<i32>, String> {
        let built = product.build(&entry, &native_options(backend, dir)).map_err(|e| format!("{e:?}"))?;
        let out = Command::new(&built.executable).output().unwrap();
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
        Ok(out.status.code())
    };
    let edits = [(Some("hook_a"), Some("hook_b")), (None, Some("hook_a")), (Some("hook_a"), None)];
    let mut built = vec![];
    let mut expected = vec![];
    for (from, to) in edits {
        let dir = tempfile::tempdir().unwrap();
        let base = Driver::default().check(hook_request(from, dir.path()), &mut Meter::new(false)).unwrap();
        assert!(base.accepted(), "{:?} {:?}", base.diagnostics, base.incomplete);
        run(&base, dir.path()).unwrap();
        let edited = Driver::default().check(hook_request(to, dir.path()), &mut Meter::new(false)).unwrap();
        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = Driver::default().check(hook_request(to, fresh_dir.path()), &mut Meter::new(false)).unwrap();
        let exit = Ok(Some(if to.is_some() { 8 } else { 7 }));
        assert_eq!(run(&fresh, fresh_dir.path()), exit, "{from:?} -> {to:?}: fresh");
        built.push((from, to, edited.rechecked.contains(&reader), run(&edited, dir.path())));
        expected.push((from, to, true, exit));
    }
    assert_eq!(built, expected);
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_hook_selection_rebuilds_like_fresh_cranelift() { hook_selection(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_hook_selection_rebuilds_like_fresh_llvm() { hook_selection(Backend::Llvm); }
