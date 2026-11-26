// Installation prebuild (prebuild.rs). Oracle for every case: the same
// program built over a std image nothing was prebuilt into, and the objects a
// build reports compiling.
fn prebuild_std(dir: &Path) -> Arc<CoreImage> {
    PackageProduct::bundled_std(dir, &mut Meter::new(false))
        .unwrap()
        .image
}
fn prebuild_libraries(image: &CoreImage) -> Vec<PackageId> {
    let directory = image.directory().unwrap();
    directory.inputs.packages.iter().map(|p| p.package.clone()).collect()
}
/// The corpus's bfs-matched, as the M1 harness lists it.
fn prebuild_corpus(image: &Arc<CoreImage>, store: &Path) -> (Arc<CheckProduct>, Option<DefPath>) {
    let m1 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/programs/m1");
    let source = |file: PathBuf, module: &str| hirsel_core_resolve::ModuleSource {
                    foreign: None,
                    captured: None,
        file,
        modules: vec![module.into()],
        signatures: false,
    };
    let libraries = prebuild_libraries(image);
    let packages = vec![
        PackageManifest {
                    namespace: None,
            package: package("bfs"),
            sources: vec![
                source(m1.join("bfs/main.hirsel"), "main"),
                source(m1.join("bfs/matched.hirsel"), "matched"),
            ],
            dependencies: libraries.iter().cloned().chain([package("m1")]).collect(),
            toolchain: false,
        },
        PackageManifest {
                    namespace: None,
            package: package("m1"),
            sources: vec![source(m1.join("support.hirsel"), "support")],
            dependencies: libraries,
            toolchain: false,
        },
    ];
    prebuild_check(image, store, packages, Some("bfs@1/main/main"))
}
/// A program no corpus holds.
fn prebuild_outside(image: &Arc<CoreImage>, store: &Path) -> (Arc<CheckProduct>, Option<DefPath>) {
    let dir = store;
    let app = dir.join("source");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] {\n    let s = \"left right\"\n    let Some((a, b)) = string.split_once(s, \" \") else {\n        return Error(Exit(status: 2u8))\n    }\n    if string.starts_with(s, a) && string.ends_with(s, b) && string.find(s, 0, \"right\") == Some(5) {\n        Ok(())\n    } else {\n        Error(Exit(status: 1u8))\n    }\n}\n",
    )
    .unwrap();
    let mut manifest = PackageManifest::directory(&app, package("app"), false).unwrap();
    manifest.dependencies.extend(prebuild_libraries(image));
    prebuild_check(image, store, vec![manifest], Some("app@1/main/main"))
}
fn prebuild_check(
    image: &Arc<CoreImage>,
    store: &Path,
    packages: Vec<PackageManifest>,
    entry: Option<&str>,
) -> (Arc<CheckProduct>, Option<DefPath>) {
    let entry = entry.map(|entry| DefPath::from_full(entry).unwrap());
    let request = CheckRequest {
        manifest: None,
        inputs: hirsel_core_resolve::CorpusManifest {
            core: image.directory().unwrap().inputs.core.clone(),
            packages,
        },
        store: store.to_owned(),
        library: LibraryInput::Image(Arc::clone(image)),
        entry: entry.clone(),
    };
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    (product, entry)
}
/// The plain library instances a build reaches, and how many it compiled.
fn plain_library_objects(product: &CheckProduct, entry: &Option<DefPath>, options: &NativeOptions) -> (usize, usize) {
    let entry = entry.as_ref().unwrap();
    let plain: Vec<_> = product
        .emit_reachable(entry, options)
        .unwrap()
        .into_iter()
        .filter(|o| {
            o.instance.dictionaries.is_empty()
                && o.instance.closures.is_empty()
                && !product.definitions.contains_key(&o.instance.definition)
                && !hirsel_core_front::is_glue(&o.instance.definition)
        })
        .collect();
    (plain.len(), plain.iter().filter(|o| o.compiled).count())
}

/// Law: reinstalling the image's unchanged prebuild is a no-op: every
/// development and recipe object is already stored, so nothing compiles.
#[test]
#[ignore = "explicit native integration target"]
fn a_prebuilt_image_prebuilds_again_without_compiling() {
    let dir = tempfile::tempdir().unwrap();
    let prebuilt = prebuild_std(&dir.path().join("prebuilt"));
    let (empty, _) = prebuild_check(&prebuilt, &dir.path().join("prebuild"), vec![], None);
    let host = native_options(Backend::Cranelift, &dir.path().join("prebuild"));
    let first = empty
        .prebuild(
            host.target.clone(),
            host.runtime.clone(),
            host.link.clone(),
            &dir.path().join("first"),
        )
        .unwrap();
    assert!(first.compiled > 0, "{first:?}");
    let second = empty
        .prebuild(
            host.target,
            host.runtime,
            host.link,
            &dir.path().join("second"),
        )
        .unwrap();
    assert_eq!(second.instances, first.instances);
    assert_eq!(second.skipped, first.skipped);
    assert_eq!(second.compiled, 0, "{second:?}");
    assert_eq!(second.recipe_compiled, 0, "{second:?}");
}

/// Law: after a prebuild, the first development build of any program over
/// that image compiles no plain library object, and links the
/// executable a build over an image nothing was prebuilt into links; a build
/// with other options keys its own objects and compiles them. Mutants: no
/// prebuild, or one emitting only what a corpus reaches, red on the first
/// count for the program outside it; objects keyed apart from a program's
/// options, red on the first count; prebuilt bytes a program would not
/// emit, red on the executable; one prebuilt set for every option set, red
/// on the last count.
#[test]
#[ignore = "explicit native integration target"]
fn a_prebuilt_image_compiles_no_plain_library_object() {
    let dir = tempfile::tempdir().unwrap();
    let prebuilt = prebuild_std(&dir.path().join("prebuilt"));
    let fresh = prebuild_std(&dir.path().join("fresh"));
    let options = |name: &str| native_options(Backend::Cranelift, &dir.path().join(name));
    let (empty, _) = prebuild_check(&prebuilt, &dir.path().join("prebuild"), vec![], None);
    let host = options("prebuild");
    let report = empty
        .prebuild(host.target, host.runtime, host.link, &dir.path().join("prebuild"))
        .unwrap();
    assert!(report.compiled > 0 && report.compiled == report.instances, "{report:?}");
    for (name, program) in [
        ("corpus", prebuild_corpus as fn(&Arc<CoreImage>, &Path) -> _),
        ("outside", prebuild_outside),
    ] {
        let mut first = options(&format!("{name}-first"));
        let (product, entry) = program(&prebuilt, &dir.path().join(format!("{name}-first")));
        let (reached, compiled) = plain_library_objects(&product, &entry, &first);
        assert!(reached > 0, "{name}: the program reaches plain library code");
        assert_eq!(compiled, 0, "{name}: the first build after a prebuild");
        first.output = dir.path().join(format!("{name}-prebuilt"));
        let built = std::fs::read(product.build(entry.as_ref().unwrap(), &first).unwrap().executable).unwrap();
        let mut without = options(&format!("{name}-fresh"));
        let (product, entry) = program(&fresh, &dir.path().join(format!("{name}-fresh")));
        without.output = dir.path().join(format!("{name}-without"));
        let expected = std::fs::read(product.build(entry.as_ref().unwrap(), &without).unwrap().executable).unwrap();
        assert!(built == expected, "{name}: the executable a build without the prebuild links");
    }
    // The charge report's observation mode is in every object's key; debug
    // lines are a link option only (ddf2a345).
    let mut other = options("other");
    other.logical_charge_report = Some(dir.path().join("other-charges"));
    let (product, entry) = prebuild_corpus(&prebuilt, &dir.path().join("other"));
    let (reached, compiled) = plain_library_objects(&product, &entry, &other);
    assert_eq!(compiled, reached, "other options compile their own objects");
}
