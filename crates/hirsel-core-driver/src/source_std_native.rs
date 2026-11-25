/// A core product under std checked from source (`--source-std`): a std
/// program's closed tuple row carries std's codec hook, so a core template
/// (`core@1/map/put_absent`) instantiated at it must find the same row in
/// core's unit. Oracle: the program reads back the 7 it stored and exits 7.
#[test]
#[ignore = "explicit native integration target"]
fn core_product_under_source_std_builds_core_templates_at_std_rows() {
    const MAIN: &str = "pub fn main()->Result[(),Exit]{
    let mut values: Map[(u64, u64), u8] = map.new()
    map.insert(mut values, (2u64, 3u64), 7u8)
    if option.unwrap(map.get(values, (2u64, 3u64))) == 7u8 { Error(Exit(status: 7u8)) } else { Error(Exit(status: 1u8)) }
}";
    let req = Req {
        product: Product::Core,
        ..build(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("main", "main.hirsel", false, lit(MAIN))]),
        ])
    };
    let dir = tempfile::tempdir().unwrap();
    let inputs = materialize(&req, false, &dir.path().join("checkout"), &mut BTreeSet::new());
    let options = NativeOptions {
        cas: dir.path().join("cas"),
        output: dir.path().join("program"),
        ..native_options(Backend::Cranelift, dir.path())
    };
    let answer = request(&mut Driver::default(), &req, inputs, dir.path().join("store"), Some(options));
    assert_eq!(
        answer.native.as_ref().and_then(|native| native.get("exit")),
        Some(&serde_json::json!(7)),
        "{answer:#?}"
    );
}
