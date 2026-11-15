// A development object is keyed by its module's hash, `ir_hash`, and the
// glue bodies it leaves to an earlier unit. Oracle for every case: a fresh
// emission of the same request into empty stores, from the same source
// files (debug lines name them). An object's bytes embed its key.
fn object_request(source: &str, dir: &Path, store: &str) -> CheckRequest {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(source, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    request.store = dir.join(store);
    request
}
/// Each reached instance's object: whether it was compiled, and its bytes
/// and debug rows.
fn objects_of(
    product: &CheckProduct,
    options: &NativeOptions,
) -> BTreeMap<String, (bool, Vec<u8>, Vec<u8>)> {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    product
        .emit_reachable(&entry, options)
        .unwrap()
        .into_iter()
        .map(|o| {
            let debug = encode(&o.debug).unwrap();
            (format!("{:?}", o.instance), (o.compiled, o.bytes, debug))
        })
        .collect()
}
fn fresh_objects(
    source: &str,
    dir: &Path,
    options: &NativeOptions,
    n: usize,
) -> BTreeMap<String, (Vec<u8>, Vec<u8>)> {
    let product = Driver::default()
        .check(
            object_request(source, dir, &format!("fresh{n}")),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let options = NativeOptions {
        store: dir.join(format!("fresh-native{n}")),
        debug_lines: options.debug_lines,
        ..native_options(options.backend, dir)
    };
    objects_of(&product, &options)
        .into_iter()
        .inspect(|(_, (compiled, _, _))| assert!(compiled))
        .map(|(i, (_, bytes, debug))| (i, (bytes, debug)))
        .collect()
}
fn without_compiled(
    objects: &BTreeMap<String, (bool, Vec<u8>, Vec<u8>)>,
) -> BTreeMap<String, (Vec<u8>, Vec<u8>)> {
    objects
        .iter()
        .map(|(i, (_, bytes, debug))| (i.clone(), (bytes.clone(), debug.clone())))
        .collect()
}
fn compiled_names(objects: &BTreeMap<String, (bool, Vec<u8>, Vec<u8>)>) -> Vec<&str> {
    objects
        .iter()
        .filter(|(_, (compiled, _, _))| *compiled)
        .map(|(i, _)| i.as_str())
        .collect()
}
const SHAPE: &str = "type P { a: u8 }\nfn id[T](take x: T) -> T { x }\nfn get(p: P) -> u8 { p.a }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(id(P(a: 7u8))))) }\n";

/// A request served from the store builds exactly the fresh request's
/// objects, a generic and a non-generic instance among them, on both
/// development engines, with debug lines off and on, under this process's
/// LLVM facts setting (a key input). The second request changes a field of
/// the shape `id[P]` is materialised with: its instance key and its generic
/// body's key stay equal, so only its module's hash can miss.
#[test]
#[ignore = "explicit native integration target"]
fn dev_objects_served_from_the_store_are_fresh_emissions() {
    let shaped = SHAPE
        .replace("{ a: u8 }", "{ a: u8, b: u64 }")
        .replace("P(a: 7u8)", "P(a: 7u8, b: 1u64)");
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for debug_lines in [false, true] {
            let case = format!("{backend:?} debug lines {debug_lines}");
            let dir = tempfile::tempdir().unwrap();
            let options = NativeOptions {
                debug_lines,
                ..native_options(backend, dir.path())
            };
            let check = |source: &str| {
                let product = Driver::default()
                    .check(
                        object_request(source, dir.path(), "store"),
                        &mut Meter::new(false),
                    )
                    .unwrap();
                assert!(product.accepted(), "{case}: {:?}", product.diagnostics);
                product
            };
            let cold = objects_of(&check(SHAPE), &options);
            let fresh = fresh_objects(SHAPE, dir.path(), &options, 0);
            assert_eq!(without_compiled(&cold), fresh, "{case}: cold");
            let generic = cold.keys().find(|i| i.contains("name: \"id\"")).unwrap();
            assert!(cold.keys().any(|i| i.contains("name: \"get\"")), "{case}");
            let served = check(SHAPE);
            assert!(served.rechecked.is_empty(), "{case}");
            let hit = objects_of(&served, &options);
            assert_eq!(compiled_names(&hit), Vec::<&str>::new(), "{case}: hit");
            assert_eq!(without_compiled(&hit), fresh, "{case}: hit");
            let edited = objects_of(&check(&shaped), &options);
            assert!(
                compiled_names(&edited).contains(&generic.as_str()),
                "{case}: the reshaped generic misses"
            );
            assert_eq!(
                without_compiled(&edited),
                fresh_objects(&shaped, dir.path(), &options, 1),
                "{case}: edited"
            );
        }
    }
}

/// Law: an LLVM object's key takes the content of the libLLVM the loader
/// opens, not its name or version: another library misses every object, the
/// same one hits. Two stand-ins one byte apart play the libraries whose
/// content the key takes; the real one emits. Oracle: a fresh emission into
/// empty stores under the same stand-in (an object's bytes embed its key).
#[test]
#[ignore = "explicit native integration target"]
fn another_libllvm_misses_every_llvm_object() {
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Llvm, dir.path());
    let library = |name: &str, last: u8| {
        let path = dir.path().join(name).join("libLLVM.so.23.1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = vec![0x5a; 4096];
        bytes[4095] = last;
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let (first, second) = (library("first", 0), library("second", 1));
    let product = Driver::default()
        .check(
            object_request(SHAPE, dir.path(), "store"),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let under =
        |library: &Path, run: &mut dyn FnMut() -> BTreeMap<String, (bool, Vec<u8>, Vec<u8>)>| {
            crate::tool_inputs::LLVM_LIBRARY.set(Some(library.to_owned()));
            let objects = run();
            crate::tool_inputs::LLVM_LIBRARY.set(None);
            objects
        };
    let fresh = |library: &Path, n| {
        let mut fresh = || {
            fresh_objects(SHAPE, dir.path(), &options, n)
                .into_iter()
                .map(|(i, (bytes, debug))| (i, (true, bytes, debug)))
                .collect()
        };
        without_compiled(&under(library, &mut fresh))
    };
    let cold = under(&first, &mut || objects_of(&product, &options));
    assert!(!cold.is_empty());
    assert_eq!(compiled_names(&cold).len(), cold.len(), "cold");
    assert_eq!(without_compiled(&cold), fresh(&first, 0), "cold");
    let hit = under(&first, &mut || objects_of(&product, &options));
    assert_eq!(
        compiled_names(&hit),
        Vec::<&str>::new(),
        "the same library hits"
    );
    assert_eq!(without_compiled(&hit), fresh(&first, 1), "hit");
    let other = under(&second, &mut || objects_of(&product, &options));
    assert_eq!(
        compiled_names(&other),
        compiled_names(&cold),
        "another library misses every object"
    );
    assert_eq!(
        without_compiled(&other),
        fresh(&second, 2),
        "another library"
    );
}

/// A valid body record whose stored module hash differs, behind the live
/// row: the next request takes the record and misses that body's object.
#[test]
#[ignore = "explicit native integration target"]
fn a_record_with_another_ir_hash_misses_its_object() {
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    let check = || {
        Driver::default()
            .check(
                object_request(SHAPE, dir.path(), "store"),
                &mut Meter::new(false),
            )
            .unwrap()
    };
    let cold = objects_of(&check(), &options);
    assert_eq!(compiled_names(&cold).len(), cold.len());
    let path = DefPath::from_full("app@1/main/get").unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    let (key, record) = check_index::ProgramIndex::open(&store)
        .unwrap()
        .live(&path)
        .unwrap();
    // The record ends with the hash of its module.
    let mut payload = store.get(record).unwrap().unwrap();
    *payload.last_mut().unwrap() ^= 1;
    let disk = decode::<DiskCheck>(&payload).unwrap();
    assert_eq!(disk.definition, path);
    let (forged, bytes) = Store::frame(&payload);
    store.write_framed(forged, &bytes).unwrap();
    check_index::forge(&store, |_, leaves| {
        for (_, rows) in leaves {
            for (p, row) in rows {
                if *p == path {
                    *row = check_index::Row::Live { key, body: forged };
                }
            }
        }
    });
    let served = check();
    assert!(served.rechecked.is_empty());
    assert_eq!(served.demand(&path).unwrap().record, forged);
    let names = objects_of(&served, &options);
    let compiled = compiled_names(&names);
    assert_eq!(compiled.len(), 1, "{compiled:?}");
    assert!(compiled[0].contains("name: \"get\""), "{compiled:?}");
}

/// A newly reached instance that sorts first takes over the key glue
/// (`$key_hash`, `$key_fold`, `$key_equal`) `map.insert` defined: `map.get`
/// now defines it, and `map.insert`'s object, its module and key unchanged,
/// must be the one that leaves it out.
#[test]
#[ignore = "explicit native integration target"]
fn glue_taken_over_by_a_first_sorting_instance_leaves_the_old_owner() {
    let b = r#"fn b() -> u64 {
  let mut m: Map[String, u64] = map.new()
  let _ = map.insert(mut m, result.unwrap(string.from("k")), 2u64)
  map.len(m)
}
"#;
    let base = format!(
        "{b}pub fn main() -> Result[(), Exit] {{ if b() == 1 {{ Ok(()) }} else {{ Error(Exit(status: 3)) }} }}\n"
    );
    let both = format!(
        "fn a() -> u64 {{\n  let m: Map[String, u64] = map.new()\n  match map.get(m, \"k\") {{ Some(v) => v, None => 0 }}\n}}\n{b}pub fn main() -> Result[(), Exit] {{ if b() + a() == 1 {{ Ok(()) }} else {{ Error(Exit(status: 3)) }} }}\n"
    );
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let check = |source: &str| {
        let product = Driver::default()
            .check(
                object_request(source, dir.path(), "store"),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        product
    };
    let cold = check(&base);
    run_clean(&cold.build(&entry, &options).unwrap().executable, 0);
    let product = check(&both);
    let edited = objects_of(&product, &options);
    let insert = edited
        .iter()
        .find(|(i, _)| i.contains("name: \"insert\""))
        .unwrap();
    assert!(insert.1.0, "the old owner's object misses");
    assert_eq!(
        without_compiled(&edited),
        fresh_objects(&both, dir.path(), &options, 0)
    );
    run_clean(&product.build(&entry, &options).unwrap().executable, 0);
}
