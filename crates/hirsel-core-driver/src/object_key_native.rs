// A development object is keyed by its instance's identity and its module's
// hash, `ir_hash`. Oracle for every case: a fresh
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
/// and debug rows: those in hand, or a stored object's from its record, as
/// the link reads them.
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
            let debug = match (&o.debug, &o.debug_record) {
                (None, Some((store, address))) => {
                    crate::native_object::load_debug(store, *address).unwrap().unwrap()
                }
                (debug, _) => debug.clone().unwrap(),
            };
            let debug = encode(&debug).unwrap();
            (format!("{:?}", o.instance), (o.compiled, std::fs::read(&o.path).unwrap(), debug))
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
/// development engines. One native store serves debug lines off, on and off
/// again: every object carries its debug rows, since a native build writes
/// detached DWARF and debug lines only embed it at the link (cli.md), so
/// only the cold request misses and the setting changes no object.
/// A last request changes a field of the shape `id[P]` is materialised with:
/// the shape's closure, and so the instance's identity, changes with it, so
/// the instance misses under a new identity.
#[test]
#[ignore = "explicit native integration target"]
fn dev_objects_served_from_the_store_are_fresh_emissions() {
    let shaped = SHAPE
        .replace("{ a: u8 }", "{ a: u8, b: u64 }")
        .replace("P(a: 7u8)", "P(a: 7u8, b: 1u64)");
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let dir = tempfile::tempdir().unwrap();
        let check = |source: &str| {
            let product = Driver::default()
                .check(
                    object_request(source, dir.path(), "store"),
                    &mut Meter::new(false),
                )
                .unwrap();
            assert!(product.accepted(), "{backend:?}: {:?}", product.diagnostics);
            product
        };
        let options = |debug_lines| NativeOptions {
            debug_lines,
            ..native_options(backend, dir.path())
        };
        let mut generic = String::new();
        for (n, debug_lines) in [false, true, false].into_iter().enumerate() {
            let case = format!("{backend:?} request {n}, debug lines {debug_lines}");
            let product = check(SHAPE);
            assert_eq!(product.rechecked.is_empty(), n > 0, "{case}");
            let objects = objects_of(&product, &options(debug_lines));
            let compiled = compiled_names(&objects);
            if n == 0 {
                assert_eq!(compiled.len(), objects.len(), "{case}: misses");
            } else {
                assert_eq!(compiled, Vec::<&str>::new(), "{case}: hits");
            }
            let fresh = fresh_objects(SHAPE, dir.path(), &options(debug_lines), n);
            assert_eq!(without_compiled(&objects), fresh, "{case}");
            generic = objects.keys().find(|i| i.contains("name: \"id\"")).unwrap().clone();
            assert!(objects.keys().any(|i| i.contains("name: \"get\"")), "{case}");
        }
        let edited = objects_of(&check(&shaped), &options(false));
        let reshaped = edited.keys().find(|i| i.contains("name: \"id\"")).unwrap();
        assert_ne!(reshaped, &generic, "{backend:?}: the reshaped generic keeps its identity");
        assert!(
            compiled_names(&edited).contains(&reshaped.as_str()),
            "{backend:?}: the reshaped generic misses"
        );
        assert_eq!(
            without_compiled(&edited),
            fresh_objects(&shaped, dir.path(), &options(false), 3),
            "{backend:?}: edited"
        );
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

/// The child half of `llvm_facts_setting_misses_and_hits_across_processes`:
/// with `OBJECT_KEY_FACTS_CHILD` naming a directory, emit SHAPE on LLVM
/// development through that directory's stores under this process's facts
/// setting and write the objects to `objects-<n>`.
#[test]
#[ignore = "explicit native integration target"]
fn llvm_facts_child() {
    let Some(dir) = std::env::var_os("OBJECT_KEY_FACTS_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let n = std::env::var("OBJECT_KEY_FACTS_RUN").unwrap();
    let product = Driver::default()
        .check(object_request(SHAPE, &dir, "store"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let objects = objects_of(&product, &native_options(Backend::Llvm, &dir));
    std::fs::write(dir.join(format!("objects-{n}")), encode(&objects).unwrap()).unwrap();
}

/// The LLVM facts setting is read once per process, so three child
/// processes share one store with facts on, off and on: the changed setting
/// misses every object, the same setting hits the first process's objects.
#[test]
#[ignore = "explicit native integration target"]
fn llvm_facts_setting_misses_and_hits_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let mut runs = vec![];
    for (n, facts) in [true, false, true].into_iter().enumerate() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--ignored", "--exact", "native::tests::llvm_facts_child"])
            .env("OBJECT_KEY_FACTS_CHILD", dir.path())
            .env("OBJECT_KEY_FACTS_RUN", n.to_string());
        if facts {
            child.env_remove("HIRSEL_LLVM_NO_FACTS");
        } else {
            child.env("HIRSEL_LLVM_NO_FACTS", "1");
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        let objects: BTreeMap<String, (bool, Vec<u8>, Vec<u8>)> =
            decode(&std::fs::read(dir.path().join(format!("objects-{n}"))).unwrap()).unwrap();
        runs.push(objects);
    }
    for (n, objects) in runs.iter().enumerate() {
        let compiled = compiled_names(objects).len();
        assert_eq!(compiled, if n < 2 { objects.len() } else { 0 }, "run {n}");
    }
    assert_eq!(without_compiled(&runs[2]), without_compiled(&runs[0]));
}

/// A valid body record whose module part is another module, behind the live
/// row: the module `get` was checked to in a variant program, under `get`'s
/// own head. The next request takes the record and misses exactly that
/// body's object, on both development engines, and the objects it builds are
/// the same product's emissions into an empty native store.
#[test]
#[ignore = "explicit native integration target"]
fn a_body_record_with_another_module_part_misses_its_object() {
    let path = DefPath::from_full("app@1/main/get").unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let dir = tempfile::tempdir().unwrap();
        let options = native_options(backend, dir.path());
        let check = |source: &str, store: &str| {
            let product = Driver::default()
                .check(
                    object_request(source, dir.path(), store),
                    &mut Meter::new(false),
                )
                .unwrap();
            assert!(product.accepted(), "{:?}", product.diagnostics);
            product
        };
        let cold = objects_of(&check(SHAPE, "store"), &options);
        assert_eq!(compiled_names(&cold).len(), cold.len());
        let variant = check(&SHAPE.replace("{ p.a }", "{ p.a + 0u8 }"), "variant");
        let other = encode(variant.definitions[&path].ir().unwrap()).unwrap();
        let store = Store::new(&dir.path().join("store")).unwrap();
        let (key, record) = check_index::ProgramIndex::open(&store)
            .unwrap()
            .live(&path)
            .unwrap();
        let head = store.get_body_head(record).unwrap().unwrap().head().to_vec();
        let (forged, part, bytes) = Store::frame_body(&head, |b| {
            b.extend_from_slice(&other);
            Ok(())
        })
        .unwrap();
        assert_ne!(part, check(SHAPE, "store").definitions[&path].ir_hash());
        store.write_framed(forged, &bytes).unwrap();
        check_index::forge(&store, |_, leaves| {
            for (_, rows) in leaves {
                for (p, row) in rows {
                    if *p == path {
                        *row = check_index::Row { key, body: forged };
                    }
                }
            }
        });
        let served = check(SHAPE, "store");
        assert!(served.rechecked.is_empty());
        assert_eq!(served.demand(&path).unwrap().record, forged);
        let names = objects_of(&served, &options);
        let compiled = compiled_names(&names);
        assert_eq!(compiled.len(), 1, "{backend:?}: {compiled:?}");
        assert!(compiled[0].contains("name: \"get\""), "{compiled:?}");
        let empty = NativeOptions {
            store: dir.path().join("empty-native"),
            ..native_options(backend, dir.path())
        };
        assert_eq!(
            without_compiled(&names),
            without_compiled(&objects_of(&served, &empty)),
            "{backend:?}"
        );
    }
}

/// Key glue (`$key_place`, `$key_class`, `$key_equal`) is an instance of its own,
/// so a newly reached instance that sorts first and calls the same glue
/// (`map.get` beside `map.insert`) changes no existing object: every object
/// the first build made, `map.insert`'s and the glue's among them, hits, on
/// both development engines.
#[test]
#[ignore = "explicit native integration target"]
fn a_new_caller_of_shared_glue_leaves_every_existing_object_a_hit() {
    let b = r#"fn b() -> u64 {
  let mut m: Map[String, u64] = map.new()
  let _ = map.insert(mut m, string.from("k"), 2u64)
  map.len(m)
}
"#;
    let base = format!(
        "{b}pub fn main() -> Result[(), Exit] {{ if b() == 1 {{ Ok(()) }} else {{ Error(Exit(status: 3)) }} }}\n"
    );
    let both = format!(
        "fn a() -> u64 {{\n  let m: Map[String, u64] = map.new()\n  match map.get(m, \"k\") {{ Some(v) => v, None => 0 }}\n}}\n{b}pub fn main() -> Result[(), Exit] {{ if b() + a() == 1 {{ Ok(()) }} else {{ Error(Exit(status: 3)) }} }}\n"
    );
    for backend in [Backend::Cranelift, Backend::Llvm] {
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(backend, dir.path());
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
    let first = objects_of(&cold, &options);
    assert!(first.keys().any(|i| i.contains("name: \"$key_equal\"")), "{backend:?}: {first:?}");
    let product = check(&both);
    let edited = objects_of(&product, &options);
    // `b`'s source is unchanged, but `main`, which calls `a`, is edited.
    for (instance, (compiled, _, _)) in &edited {
        if first.contains_key(instance) && !instance.contains("name: \"main\"") {
            assert!(!compiled, "{backend:?}: {instance} misses");
        }
    }
    assert_eq!(
        without_compiled(&edited),
        fresh_objects(&both, dir.path(), &options, 0)
    );
    run_clean(&product.build(&entry, &options).unwrap().executable, 0);
    }
}
