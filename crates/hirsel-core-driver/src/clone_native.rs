fn clone_request(source: &str, dir: &Path) -> CheckRequest {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request("pub fn main() -> Result[(), Exit] { let p = probe.Probe(value: \"kept\"); let q: probe.Probe[String] = result.unwrap(clone(p)); if q.value == \"kept\" { Ok(()) } else { Error(Exit(status: 7)) } }", dir);
    let mut core = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let file = dir.join("probe.hirsel");
    std::fs::write(&file, source).unwrap();
    core.sources.push(hirsel_core_resolve::ModuleSource { file, modules: vec!["probe".into()], signatures: true });
    request.inputs.packages[1] = core;
    request
}
const CLONE_PROBE: &str = "pub type Probe[T] { value: T }\nclone[T](p: Probe[T]) { let copied: Result[T, Capacity] = clone(p.value); match take copied { Ok(take x) => Ok(Probe(value: take x)), Error(c) => Error(c) } }\npub fn untouched() -> u64 { 42u64 }\n";

#[test]
fn clone_body_keys_preserve_type_identity_across_edits_and_reordering() {
    let dir = tempfile::tempdir().unwrap();
    let path = DefPath::from_full("core@1/probe/Probe.$clone").unwrap();
    let main = DefPath::from_full("app@1/main/main").unwrap();
    let mut driver = Driver::default();
    let first = driver.check(clone_request(CLONE_PROBE, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(first.accepted(), "{:?} {:?}", first.diagnostics, first.incomplete);
    assert!(first.tests().next().is_none());
    assert!(first.headers[&path].has_body, "a published clone is a demanded body");
    let changed = CLONE_PROBE.replace("let copied:", "let _ = 0u64; let copied:");
    let edited = driver.check(clone_request(&changed, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(edited.accepted(), "{:?} {:?}", edited.diagnostics, edited.incomplete);
    assert_eq!(edited.rechecked, [path.clone()]);
    assert!(Arc::ptr_eq(&first.definitions[&main], &edited.definitions[&main]));
    let lines: Vec<_> = changed.lines().collect();
    let reordered = format!("{}\n{}\n{}\n", lines[2], lines[0], lines[1]);
    let reordered = Driver::default().check(clone_request(&reordered, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(reordered.accepted(), "{:?} {:?}", reordered.diagnostics, reordered.incomplete);
    assert!(reordered.rechecked.is_empty(), "{:?}", reordered.rechecked);
    assert_eq!(edited.definitions[&path].key, reordered.definitions[&path].key);
    assert_eq!(reordered.headers[&path].anchor.1, 2);
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
    assert!(reached.instances.as_ref().unwrap().iter().any(|i| i.definition == path && !i.types.is_empty()));
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
