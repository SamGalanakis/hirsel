// FAIL-2 / PROG-3: output precedes a failure at the actual callee expression.
// Both positions are hand-counted below; no evaluator-derived expected value.
fn failure_sites_rebind(backend: Backend, optimize: bool, debug_lines: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let overflow = DefPath::from_full("app@1/main/overflow").unwrap();
    let source = "fn overflow(x: u64) -> u64 {\n    x + 1\n}\npub fn main(out: Stdout) -> Result[(), Exit] {\n    result.unwrap(stdout.write(out, \"before\\n\"));\n    if overflow(18446744073709551615u64) == 0 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n";
    let check = |source: &str| {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(
            &root.join("core/core"), package("core"), true,
        ).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?} {:?}", product.diagnostics, product.incomplete, product.resources);
        product
    };
    let mut options = native_options(backend, dir.path());
    options.optimize = optimize;
    options.debug_lines = debug_lines;
    options.grant_mask = 1 << 4;
    let run = |built: &BuildProduct, line: u32| {
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(101), "{output:?}");
        assert_eq!(output.stdout, b"before\n", "{output:?}");
        assert_eq!(output.stderr, format!("Overflow at app@1/main.hirsel:{line}:5\n").as_bytes(), "{output:?}");
        assert_eq!(built.debug_lines, debug_lines);
        let bytes = std::fs::read(&built.executable).unwrap();
        use object::Object as _;
        assert_eq!(object::File::parse(bytes.as_slice()).unwrap().kind(), object::ObjectKind::Dynamic,
            "site constants must survive a PIE load bias");
        if debug_lines {
            let positions = crate::native_debug::source_positions(&bytes).unwrap();
            assert!(positions.contains(&("app@1/main.hirsel".to_string(), line, 5)),
                "debug line table lost the raising expression: {positions:?}");
        } else {
            use object::{Object as _, ObjectSection as _};
            let file = object::File::parse(bytes.as_slice()).unwrap();
            assert!(file.sections().all(|s| !s.name().unwrap().starts_with(".debug")),
                "release without line rows must not link a debug object");
            let crate::native_debug::SymbolSource::DetachedDwarf(rows) =
                crate::native_debug::resolve_symbols(&built.executable, &bytes, None).unwrap()
            else {
                panic!("missing detached source locations");
            };
            let positions = crate::native_debug::source_positions(&rows).unwrap();
            assert!(positions.contains(&("app@1/main.hirsel".to_string(), line, 5)),
                "detached line table lost the raising expression: {positions:?}");
        }
    };
    let cold = check(source).build(&entry, &options).unwrap();
    assert!(cold.compiled > 0 && cold.linked);
    run(&cold, 2);

    // A metadata-only change (chmod to the same mode moves ctime) keeps
    // the runtime bytes, so every object stays.
    let runtime = &options.runtime;
    std::fs::set_permissions(runtime, std::fs::metadata(runtime).unwrap().permissions()).unwrap();
    let warm = check(source);
    assert!(warm.rechecked.is_empty());
    let built = warm.build(&entry, &options).unwrap();
    assert_eq!(built.compiled, 0);
    assert!(!built.linked);
    run(&built, 2);

    // A rebuilt Hirsel compiler (DESIGN v4.1 3a's cross-toolchain case) owns no
    // object or executable an earlier build published.
    crate::reuse::TOOLCHAIN.set(Some(ContentHash::of_bytes(b"another compiler build")));
    let rebuilt = warm.build(&entry, &options);
    crate::reuse::TOOLCHAIN.set(None);
    let rebuilt = rebuilt.unwrap();
    assert_eq!(rebuilt.compiled, cold.compiled, "another compiler must recompile every object");
    assert!(rebuilt.linked, "another compiler must relink");
    run(&rebuilt, 2);

    // Three lines owned by a new definition, with existing item text unchanged.
    let moved = format!("fn added() -> u64 {{\n    0\n}}\n{source}");
    let product = check(&moved);
    assert!(!product.rechecked.contains(&entry));
    assert!(!product.rechecked.contains(&overflow));
    let built = product.build(&entry, &options).unwrap();
    assert_eq!(built.compiled, 0, "a line move must reuse every reached body object");
    assert!(built.linked, "current source belongs in the fresh link product");
    run(&built, 5);

    if matches!(backend, Backend::Cranelift) && !optimize {
        let code = crate::code_store::CodeStore::new(dir.path().join("recipe-code"));
        let build_recipe = |source: &str| {
            let product = check(source);
            let reached = product.reachable(&entry, options.instance_limit).unwrap();
            product
                .publish_release(&entry, &reached, &options, &code, hirsel_core_ids::launch::DEFAULT_N_PLACE)
                .unwrap()
                .assemble(&options, reached.counters)
                .unwrap()
        };
        let cold = build_recipe(source);
        assert!(cold.compiled > 0 && cold.linked);
        run(&cold, 2);
        let relocated = build_recipe(&moved);
        assert_eq!(relocated.compiled, 0, "moved recipe sites must reuse their objects");
        assert!(relocated.linked);
        run(&relocated, 5);
    }

    if matches!(backend, Backend::Llvm) && optimize && !debug_lines && options.target == TARGET {
        let shifted = moved.replace("    x + 1", "    let _ = 0u8\n    x + 1");
        let product = check(&shifted);
        assert!(product.rechecked.contains(&overflow));
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 0);
        assert!(built.linked);
        run(&built, 6);

        let changed = shifted.replace("x + 1", "x + 0");
        let built = check(&changed).build(&entry, &options).unwrap();
        assert!(built.compiled > 0 && built.linked);
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        assert_eq!(output.stdout, b"before\n");
        assert!(output.stderr.is_empty());
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_dev() { failure_sites_rebind(Backend::Cranelift, false, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_release() { failure_sites_rebind(Backend::Cranelift, true, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_dev() { failure_sites_rebind(Backend::Llvm, false, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_release() { failure_sites_rebind(Backend::Llvm, true, true); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_release_default() { failure_sites_rebind(Backend::Cranelift, true, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_release_default() { failure_sites_rebind(Backend::Llvm, true, false); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_dev_default() { failure_sites_rebind(Backend::Cranelift, false, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_dev_default() { failure_sites_rebind(Backend::Llvm, false, false); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_dynamic_relocation_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut object = object::write::Object::new(object::BinaryFormat::Elf, object::Architecture::X86_64, object::Endianness::Little);
    let section = object.add_section(vec![], b".data".to_vec(), object::SectionKind::Data);
    object.section_mut(section).set_data(vec![0;8],8);
    let name = hirsel_core_ir::source::site_symbol(&DefPath::core("dynamic_site"));
    let symbol = object.add_symbol(object::write::Symbol { name:name.into_bytes(),value:0,size:0,kind:object::SymbolKind::Unknown,scope:object::SymbolScope::Dynamic,weak:false,section:object::write::SymbolSection::Undefined,flags:object::SymbolFlags::None });
    object.add_relocation(section,object::write::Relocation { offset:0, symbol,addend:0,flags:object::RelocationFlags::Generic { kind:object::RelocationKind::Absolute,encoding:object::RelocationEncoding::Generic,size:64 } }).unwrap();
    let input = dir.path().join("site.o"); let output = dir.path().join("site.so");
    std::fs::write(&input,object.write().unwrap()).unwrap();
    let options = native_options(Backend::Cranelift,dir.path());
    let linked = Command::new(options.link.driver()).args(["-shared","-nostdlib"]).arg(input).arg("-o").arg(&output).output().unwrap();
    assert!(linked.status.success(),"{linked:?}");
    let error = crate::native_entry::check_linked_sites(&std::fs::read(output).unwrap()).unwrap_err();
    assert!(error.contains("dynamic relocation against site symbol"),"{error}");
}

// An executable names its sources by package (FAIL-2's report reads
// `std@1/list.hirsel`), never by where the core product is installed or where
// the project lives: the same program built under two cache roots and two
// project directories, of different lengths, is the same executable.
fn failure_sites_relocated(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let list = std::fs::read_to_string(libraries.join("std/list.hirsel")).unwrap();
    let mut lines = list.lines().enumerate();
    lines
        .find(|(_, line)| line.starts_with("fn check_index["))
        .expect("std/list.hirsel must define check_index");
    let (line, column) = lines
        .take_while(|(_, line)| *line != "}")
        .find_map(|(line_number, line)| {
            line.split_once("view xs[i]")
                .map(|(prefix, _)| (line_number + 1, prefix.chars().count() + 1))
        })
        .expect("check_index must contain view xs[i]");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = "pub fn main() -> Result[(), Exit] {\n    let mut xs: List[u64] = list.new()\n    let _ = list.remove(mut xs, 3)\n    Ok(())\n}\n";
    let build = |project: &str, cache: &str| {
        let project = dir.path().join(project);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(dir.path().join(cache)).unwrap();
        std::fs::write(project.join("main.hirsel"), source).unwrap();
        let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
        let product = Driver::default().check_reachable_with_product(request, &dir.path().join(cache), &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?} {:?}", product.diagnostics, product.incomplete, product.resources);
        let mut options = native_options(backend, &project);
        options.optimize = true;
        let built = product.build(&entry, &options).unwrap();
        let output = Command::new(&built.executable).output().unwrap();
        (std::fs::read(&built.executable).unwrap(), output)
    };
    let (first, output) = build("project-a", "cache-a/hirsel/core-packages");
    let report = String::from_utf8_lossy(&output.stderr).into_owned();
    for (project, cache) in [("project-a", "cache-bbbbbbbbbbbb/hirsel/core-packages"), ("project-bbbbbbbbbbbb", "cache-a/hirsel/core-packages")] {
        let (other, _) = build(project, cache);
        assert!(other == first, "{project} under {cache} is another executable; its report: {report}");
    }
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    assert_eq!(report, format!("Bounds at std@1/list.hirsel:{line}:{column}\n"));
}

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_relocated_cranelift() { failure_sites_relocated(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_relocated_llvm() {
    failure_sites_relocated(Backend::Llvm);
    failure_sites_discarded_by_linker();
}

fn failure_sites_discarded_by_linker() {
    use hirsel_core_ir::source::{ItemSource, site_symbol};
    use hirsel_core_layout::source_positions::{POINTS_TAG, point_position};
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};

    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let overflow = DefPath::from_full("app@1/main/overflow").unwrap();
    let dead = DefPath::from_full("app@1/main/dead_coordinate").unwrap();
    const PREFIX: &str = "fn overflow(x: u64) -> u64 {\n    ";
    let source = format!(
        "{PREFIX}x + 1\n}}\npub fn main() -> Result[(), Exit] {{\n    if overflow(18446744073709551615u64) == 0 {{ Ok(()) }} else {{ Error(Exit(status: 7)) }}\n}}\n"
    );
    let product = Driver::default()
        .check(source_request(&source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let dev = native_options(Backend::Llvm, dir.path());
    let reached = product.reachable_for(&entry, &dev).unwrap();
    let inputs = reached.native.as_ref().unwrap();
    let (mut objects, _) = product.emit_reached(&entry, &reached, &dev).unwrap();
    let main = reached
        .require()
        .unwrap()
        .iter()
        .find(|id| id.definition == entry)
        .unwrap();
    let items: Vec<_> = reached
        .require()
        .unwrap()
        .iter()
        .flat_map(|id| reached.materialized.items(id).unwrap())
        .collect();
    let rows = product.source_rows(&items).unwrap();
    let mut sources = product.source_map(&rows).unwrap();
    let file = sources.items[&overflow].file;
    sources.items.insert(
        dead.clone(),
        ItemSource {
            file,
            file_offset: 1,
        },
    );

    // The only reference to line 1, column 2 (the n of fn) is in a separate
    // allocated section. LLD can discard it, even though it shares the live file.
    let mut input = object::write::Object::new(
        object::BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let section = input.add_section(
        vec![],
        b".rodata.dead_coordinate".to_vec(),
        object::SectionKind::ReadOnlyData,
    );
    input.section_mut(section).set_data(vec![0; 8], 8);
    input.add_symbol(object::write::Symbol {
        name: b"dead_coordinate_section".to_vec(),
        value: 0,
        size: 8,
        kind: object::SymbolKind::Data,
        scope: object::SymbolScope::Compilation,
        weak: false,
        section: object::write::SymbolSection::Section(section),
        flags: object::SymbolFlags::None,
    });
    let binding = input.add_symbol(object::write::Symbol {
        name: site_symbol(&dead).into_bytes(),
        value: 0,
        size: 0,
        kind: object::SymbolKind::Unknown,
        scope: object::SymbolScope::Linkage,
        weak: false,
        section: object::write::SymbolSection::Undefined,
        flags: object::SymbolFlags::None,
    });
    input
        .add_relocation(
            section,
            object::write::Relocation {
                offset: 0,
                symbol: binding,
                addend: 0,
                flags: object::RelocationFlags::Elf {
                    r_type: object::elf::R_X86_64_64,
                },
            },
        )
        .unwrap();
    let bytes = input.write().unwrap();
    let path = dir.path().join("dead.o");
    std::fs::write(&path, &bytes).unwrap();
    objects.push(ObjectProduct {
        private_variants: 0,
        instance: main.clone(),
        key: ContentHash::of_bytes(&bytes),
        identity: ContentHash::of_bytes(&bytes),
        check: crate::store::checksum(&bytes),
        bytes: Some(bytes.into()),
        path,
        store: None,
        debug: None,
        debug_record: None,
        compiled: true,
    });
    let options = NativeOptions {
        optimize: true,
        ..dev
    };
    let (abi, cleanup) = product
        .bind_entry_native(&entry, &options, inputs, &BTreeSet::new())
        .unwrap();
    let plan = crate::native_entry::Plan::new(
        main.symbol(),
        entry.to_string(),
        &abi,
        cleanup.as_ref().map(|object| object.instance.symbol()),
        false,
        &options,
    )
    .unwrap();
    objects.extend(cleanup);
    let linked = link_objects(
        &plan,
        &objects,
        &crate::foreign_archive::LinkArchives::default(),
        None,
        &options,
        ContentHash::default(),
        || Ok((std::sync::Arc::new(sources), None)),
        inputs.toolchain.image,
        inputs.recipe.clone(),
        false,
        reached.counters,
        |_| Ok(0),
        reached.address(&entry).unwrap(),
        || reached.revisions(),
        None,
    )
    .unwrap();
    let Linked::Built(built) = linked else {
        panic!("objects failed verification")
    };
    let output = Command::new(&built.executable).output().unwrap();
    assert_eq!(output.status.code(), Some(101));
    assert_eq!(output.stderr, b"Overflow at app@1/main.hirsel:2:5\n");

    let bytes = std::fs::read(&built.executable).unwrap();
    let elf = object::File::parse(bytes.as_slice()).unwrap();
    assert!(
        !elf.symbols()
            .any(|symbol| symbol.name() == Ok("dead_coordinate_section"))
    );
    let table = elf
        .symbols()
        .find(|symbol| symbol.name() == Ok("__hirsel_source_files"))
        .unwrap();
    let section = elf
        .section_by_index(table.section_index().unwrap())
        .unwrap();
    let data = section
        .data_range(table.address(), table.size())
        .unwrap()
        .unwrap();
    let word = |offset| u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()) as usize;
    assert_eq!(word(0), 40, "the only source file is main.hirsel");
    assert_eq!(word(32), 1);
    let points = data[word(16)..word(16) + word(24)]
        .strip_prefix(&[POINTS_TAG])
        .unwrap();
    assert_eq!(point_position(points, 1), None);
    assert_eq!(point_position(points, PREFIX.len()), Some((2, 5)));
}

// A function value's adapter is generated code, so a core callee that fails through
// it reports the nearest non-core caller's site (driver README): the call through the
// value, not the place the value was named. Every unit that carries one adapter
// defines it weak, so a site read from the adapter's own text would be whichever
// maker's copy the linker kept: with two makers, one of them never run. Positions
// are hand-counted in each source.
fn failure_sites_adapter(backend: Backend, optimize: bool, debug_lines: bool) {
    const APPLY: &str = "fn apply(f: fn(take Option[u64]) -> u64, x: Option[u64]) -> u64 {\n    f(x)\n}\n";
    let cases = [
        // One maker, a core callee: the call `f(x)`, 2:5.
        ("pub fn main() -> Result[(), Exit] {\n    let none: Option[u64] = None\n    if apply(option.unwrap, none) == 0u64 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n", 2, 5),
        // A user callee fails in its own item, 5:5.
        ("fn mine(take x: Option[u64]) -> u64 {\n    option.unwrap(x)\n}\npub fn main() -> Result[(), Exit] {\n    let none: Option[u64] = None\n    if apply(mine, none) == 0u64 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n", 5, 5),
        // Two units make one adapter and only `second` runs: still `f(x)`, 2:5.
        ("fn first() -> u64 {\n    let none: Option[u64] = None\n    apply(option.unwrap, none)\n}\nfn second() -> u64 {\n    let none: Option[u64] = None\n    apply(option.unwrap, none)\n}\npub fn main(argv: Args) -> Result[(), Exit] {\n    let v = if args.len(argv) > 5u64 { first() } else { second() }\n    if v == 0u64 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n", 2, 5),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    // A second request in the same store compiles no body: the adapter its
    // failure runs through is carried by a hit object, and reports the same site.
    for (i, (main, line, column)) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        for request_number in 0..2 {
            let case = format!("case {i}, request {request_number}");
            let mut request = source_request(&format!("{APPLY}{main}"), dir.path());
            request.inputs.packages[1] =
                PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
            let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
            assert!(product.accepted(), "{case}: {:?} {:?}", product.diagnostics, product.incomplete);
            let mut options = native_options(backend, dir.path());
            options.optimize = optimize;
            options.debug_lines = debug_lines;
            let built = product.build(&entry, &options).unwrap();
            if request_number == 1 {
                assert_eq!(built.compiled, 0, "{case}: every body is a hit");
            }
            let output = Command::new(&built.executable).output().unwrap();
            assert_eq!(output.status.code(), Some(101), "{case}: {output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                format!("Unwrap at app@1/main.hirsel:{line}:{column}\n"),
                "{case}"
            );
        }
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_adapter_cranelift_dev() { failure_sites_adapter(Backend::Cranelift, false, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_adapter_cranelift_dev_lines() { failure_sites_adapter(Backend::Cranelift, false, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_adapter_llvm_dev() { failure_sites_adapter(Backend::Llvm, false, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_adapter_llvm_release_lines() { failure_sites_adapter(Backend::Llvm, true, true); }

/// One module in two files (review 1043 F3). A native site and a diagnostic
/// name the file their item is in: a failure in the second file reports that
/// file's line, under the manifest's name for it when the request has a
/// manifest; an item moved across the files is found in its new file; and a
/// build against the primed store links what an empty store links.
fn failure_sites_two_files(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let (a, b) = (dir.path().join("a.hirsel"), dir.path().join("b.hirsel"));
    let named = |first: &str, second: &str, manifest: bool| {
        std::fs::write(&a, first).unwrap();
        std::fs::write(&b, second).unwrap();
        let mut request = source_request("", dir.path());
        request.manifest = manifest.then(|| dir.path().to_owned());
        request.inputs.packages[0].sources = [&a, &b]
            .map(|file| hirsel_core_resolve::ModuleSource {
                    foreign: false,
                    captured: None,
                file: file.clone(),
                modules: vec!["main".into()],
                signatures: false,
            })
            .to_vec();
        Driver::default().check(request, &mut Meter::new(false)).unwrap()
    };
    let check = |first: &str, second: &str| named(first, second, false);
    let options = native_options(backend, dir.path());
    let mut fresh = 0;
    let mut built = |product: &CheckProduct| {
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        let primed = product.build(&entry, &options).unwrap();
        fresh += 1;
        let mut empty = native_options(backend, dir.path());
        empty.cas = dir.path().join(format!("fresh-{fresh}"));
        empty.output = dir.path().join(format!("fresh-program-{fresh}"));
        let alone = product.build(&entry, &empty).unwrap();
        let bytes = std::fs::read(&primed.executable).unwrap();
        assert!(bytes == std::fs::read(&alone.executable).unwrap(), "primed != fresh");
        Command::new(&primed.executable).output().unwrap()
    };

    // The failing item is item 0 of the second file; the first file's item 0
    // starts two lines lower than it, so the wrong file gives line 4; the
    // first file has as many items, so the wrong one is found, not refused.
    const OVERFLOW: &str = "fn overflow(x: u64) -> u64 {\n    x + 1\n}\npub fn main() -> Result[(), Exit] {\n    if overflow(18446744073709551615u64) == 0 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n";
    let output = built(&check("\n\nfn g() -> u64 {\n    0\n}\nfn h() -> u64 {\n    1\n}\n", OVERFLOW));
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "Overflow at app@1/main.hirsel:2:5\n");
    let output = built(&named("\n\nfn g() -> u64 {\n    0\n}\nfn h() -> u64 {\n    1\n}\n", OVERFLOW, true));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "Overflow at b.hirsel:2:5\n");

    // A refusal in item 0 of the second file: its file's name and offset.
    let refused = named("\n\nfn g() -> u64 {\n    0\n}\n", "fn y() -> u64\n", true);
    let diagnostics = refused.machine_diagnostics().unwrap();
    let sites: Vec<_> = diagnostics.iter().map(|d| (d.rule, d.primary.module.as_str(), d.primary.span.start)).collect();
    assert_eq!(sites, [("LANG-5", "b.hirsel", 13)]);

    // The reviewer's move: f leaves the first file for the second, before main.
    const MAIN: &str = "pub fn main() -> Result[(), Exit] { Error(Exit(status: f())) }\n";
    let output = built(&check("fn f() -> u8 { 7u8 }\nfn g() -> u8 { 8u8 }\n", MAIN));
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let output = built(&check("fn g() -> u8 { 8u8 }\n", &format!("fn f() -> u8 {{ 7u8 }}\n{MAIN}")));
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_two_files_cranelift() { failure_sites_two_files(Backend::Cranelift); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_two_files_llvm() { failure_sites_two_files(Backend::Llvm); }
