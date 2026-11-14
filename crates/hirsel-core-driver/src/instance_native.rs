// Stored generic instances use the same layout identities and ABI in both engines.
fn anchored_instance_key(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/instance-anchored.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let built = product
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(backend, dir.path()),
        )
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_instance_key_cranelift() {
    anchored_instance_key(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_instance_key_llvm() {
    anchored_instance_key(Backend::Llvm);
}

// Stored generic instances use the same layout identities and ABI in both engines.
fn grouped_region_calls(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    for source in [
        include_str!("../../../core/front/cases/body/call-region-grouped.hirsel"),
        include_str!("../../../core/front/cases/body/callable-region-take-result.hirsel"),
    ] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let built = product
            .build(
                &DefPath::from_full("app@1/main/main").unwrap(),
                &native_options(backend, dir.path()),
            )
            .unwrap();
        run_clean(&built.executable, 0);
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn grouped_region_calls_cranelift() {
    grouped_region_calls(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn grouped_region_calls_llvm() {
    grouped_region_calls(Backend::Llvm);
}

// OWN-20 / OWN-11 (ruling 7): results anchored to a `mut` parameter, read beside
// their frozen place and lent again by each `while let` iteration, in both engines.
fn mut_anchored_results(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/own-20-mut-anchor.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let built = product
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(backend, dir.path()),
        )
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn mut_anchored_results_cranelift() {
    mut_anchored_results(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn mut_anchored_results_llvm() {
    mut_anchored_results(Backend::Llvm);
}

// Stored generic instances use the same layout identities and ABI in both engines.
fn anchored_map_rows(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    for source in [
        include_str!("../../../core/front/cases/body/map-anchored.hirsel"),
        include_str!("../../../core/front/cases/body/mut-scoped-regional-call.hirsel"),
    ] {
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let built = product
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(backend, dir.path()),
        )
        .unwrap();
    run_clean(&built.executable, 0);
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_map_rows_cranelift() {
    anchored_map_rows(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_map_rows_llvm() {
    anchored_map_rows(Backend::Llvm);
}

fn scalar_task_entry(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/scalar-task-entry.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let mut options = native_options(backend, dir.path());
    options.grant_mask = 1 << 9;
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_task_entry_cranelift() {
    scalar_task_entry(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_task_entry_llvm() {
    scalar_task_entry(Backend::Llvm);
}

fn scalar_callable_instances(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (case, check) in [
        (
            include_str!("../../../core/front/cases/body/generic-instances.hirsel"),
            "width()==72623859790382856u64 && wrapped()==72623859790382856u64 && pair()==145247719580765712u64 && swapped()==9u8 && unit()==3u8",
        ),
        (
            include_str!("../../../core/front/cases/body/generic-callable-abi.hirsel"),
            "f()==7u8 && direct()==19u64 && zero()==9u64 && nested()==23u64",
        ),
        (
            include_str!("../../../core/front/cases/body/closures-generic.hirsel"),
            "scalar()==7u64 && unit()==9u64 && pair()==13u64 && plain()==23u64",
        ),
        (
            include_str!("../../../core/front/cases/body/callable-view-payload.hirsel"),
            "call(9u64)==10u64",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{case}\npub fn main()->Result[(),Exit] {{ if {check} {{Ok(())}}else{{Error(Exit(status:1))}} }}"
        );
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let built = product
            .build(
                &DefPath::from_full("app@1/main/main").unwrap(),
                &native_options(backend, dir.path()),
            )
            .unwrap();
        run_clean(&built.executable, 0);
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_callable_instances_cranelift() {
    scalar_callable_instances(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_callable_instances_llvm() {
    scalar_callable_instances(Backend::Llvm);
}

fn scalar_key_carriage(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/scalar-key-carriage.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let built = product
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(backend, dir.path()),
        )
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_key_carriage_cranelift() {
    scalar_key_carriage(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_key_carriage_llvm() {
    scalar_key_carriage(Backend::Llvm);
}

#[test]
fn scalar_core_instances_match_closed_bodies() {
    fn close(source: &str, name: &str, parameters: &str, types: &[&str]) -> String {
        let start = source
            .find(&format!("pub fn {name}[{parameters}]"))
            .unwrap();
        let open = start + source[start..].find('{').unwrap();
        let mut depth = 0;
        let end = source[open..]
            .char_indices()
            .find_map(|(i, c)| {
                if c == '{' {
                    depth += 1;
                }
                if c == '}' {
                    depth -= 1;
                }
                (depth == 0).then_some(open + i + 1)
            })
            .unwrap();
        let source =
            source[start..end].replace(&format!("{name}[{parameters}]"), &format!("{name}_closed"));
        let mut out = String::new();
        let mut word = String::new();
        for c in source.chars().chain(std::iter::once(' ')) {
            if c.is_alphanumeric() || c == '_' {
                word.push(c);
            } else {
                out.push_str(if types.contains(&word.as_str()) {
                    "u64"
                } else {
                    &word
                });
                word.clear();
                out.push(c);
            }
        }
        out
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("core");
    std::fs::create_dir(&core).unwrap();
    for entry in std::fs::read_dir(root.join("core/core")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "hirsel") {
            let mut text = std::fs::read_to_string(&path).unwrap();
            let closed = match path.file_stem().unwrap().to_str().unwrap() {
                "list" => close(&text, "sum", "T: integer", &["T"]),
                "map" => close(&text, "get", "K: key, V", &["K", "V"]),
                _ => String::new(),
            };
            text.push('\n');
            text.push_str(&closed);
            std::fs::write(core.join(path.file_name().unwrap()), text).unwrap();
        }
    }
    let mut request = source_request(
        "fn run(m:Map[u64,u64],xs:List[u64])->u64 {let a=list.sum(xs);let b=list.sum_closed(xs);let _=map.get(m,0u64);let _=map.get_closed(m,0u64);a+b}",
        dir.path(),
    );
    request.entry = None;
    request.inputs.packages[1] = PackageManifest::directory(&core, package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/run").unwrap(), 65536)
        .unwrap();
    assert!(
        reached.accepted(),
        "{:?}",
        reached
            .first_stops
            .iter()
            .map(|s| &s.construct)
            .collect::<Vec<_>>()
    );
    for (module, name, types) in [
        ("list", "sum", vec!["u64".to_owned()]),
        ("map", "get", vec!["u64".to_owned(), "u64".to_owned()]),
    ] {
        let definition = DefPath::from_full(&format!("core@1/{module}/{name}")).unwrap();
        let key = reached
            .instances
            .as_ref()
            .unwrap()
            .iter()
            .find(|k| k.definition == definition && k.types == types)
            .unwrap()
            .clone();
        let closed = hirsel_core_ir::InstanceKey {
            definition: DefPath::from_full(&format!("core@1/{module}/{name}_closed")).unwrap(),
            types: vec![],
            dictionaries: vec![],
        };
        let a = reached.materialized.get(&key).ir();
        let b = reached.materialized.get(&closed).ir();
        let find = |m: &Module, k: &hirsel_core_ir::InstanceKey| {
            m.functions
                .iter()
                .position(|f| &m.instances[f.instance.index()] == k)
                .unwrap()
        };
        let af = &a.functions[find(a, &key)];
        let bf = &b.functions[find(b, &closed)];
        let sa = &a.signatures[af.signature.index()];
        let sb = &b.signatures[bf.signature.index()];
        assert_eq!(sa.results, sb.results, "{name} result");
        assert_eq!(sa.dest.is_some(), sb.dest.is_some());
        assert_eq!(
            sa.params
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>(),
            sb.params
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>()
        );
        let ab = af.body.as_ref().unwrap();
        let bb = bf.body.as_ref().unwrap();
        assert_eq!(ab.slots.len(), bb.slots.len(), "{name} slots");
        assert_eq!(
            ab.insts
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>(),
            bb.insts
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>(),
            "{name} instructions"
        );
    }
}
