// A finalizer is reached through a type recipe, without a source call to it.
fn finalizer_entry(backend: Backend, carrier: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for divisor in [0, 2] {
        let dir = tempfile::tempdir().unwrap();
        let keep = match carrier {
            "plain" => "let r = result.unwrap(R(d: DIVISOR));".to_owned(),
            "list" => "let r = result.unwrap(R(d: DIVISOR)); let mut xs: List[R] = result.unwrap(list.with_capacity(1)); result.unwrap(list.push(mut xs, take r));".to_owned(),
            "record" => "let r = result.unwrap(R(d: DIVISOR)); let owner = Record(r: take r);".to_owned(),
            "variant" => "let r = result.unwrap(R(d: DIVISOR)); let owner: Variant = Keep(take r);".to_owned(),
            "option" => "let r = result.unwrap(R(d: DIVISOR)); let owner: Option[R] = Some(take r);".to_owned(),
            "box" => "let r = result.unwrap(R(d: DIVISOR)); let owner = result.unwrap(box.new(take r));".to_owned(),
            "generic_resource" => "let r: Generic[u64] = result.unwrap(Generic(d: DIVISOR, value: 11u64));".to_owned(),
            "generic" => "let r = result.unwrap(R(d: DIVISOR)); dispose(take r);".to_owned(),
            _ => unreachable!(),
        }.replace("DIVISOR", &format!("{divisor}u64"));
        let source = format!("resource type R {{ d: u64 }} drop(t) {{ let _ = divide(t.d); }}\ntype Record {{ r: R }}\ntype Variant = Empty | Keep(R)\nresource type Generic[T] {{ d: u64, value: T }} drop(t) {{ let _ = divide(t.d); }}\nfn divide(d: u64) -> u64 {{ 100u64 / d }}\nfn dispose[T](take x: T) {{}}\npub fn main() -> Result[(), Exit] {{ {keep} Error(Exit(status: 7)) }}\n");
        let source = if carrier == "generic_resource" { source } else {
            source.replace("resource type Generic[T] { d: u64, value: T } drop(t) { let _ = divide(t.d); }\n", "")
        };
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let reached = product.reachable(&entry, 64).unwrap();
        assert!(reached.accepted(), "{carrier}: {}", serde_json::to_string(&(product.machine_diagnostics().unwrap(), product.machine_incomplete().unwrap(), &reached.first_stops)).unwrap());
        let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
        let instances = reached.instances.unwrap();
        let finalizer = if carrier == "generic_resource" { "app@1/main/Generic" } else { "app@1/main/R" };
        assert!(instances.iter().any(|k| k.definition == DefPath::from_full(finalizer).unwrap()), "missing finalizer: {instances:?}");
        assert!(instances.iter().any(|k| k.definition == DefPath::from_full("app@1/main/divide").unwrap()), "missing finalizer callee: {instances:?}");
        if carrier == "generic" {
            assert!(instances.iter().any(|k| k.definition.0.name == "dispose" && !k.types.is_empty()));
        }
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(if divisor == 0 { 101 } else { 7 }), "{carrier}: {:?}", output);
        assert!(output.stdout.is_empty());
        if divisor == 0 {
            assert!(output.stderr.starts_with(b"Division"), "{:?}", output);
            assert_eq!(String::from_utf8_lossy(&output.stderr).lines().filter(|s| s.starts_with("Division")).count(), 1);
        } else {
            assert!(output.stderr.is_empty(), "{:?}", output);
        }
    }
}
macro_rules! finalizer_entries {
    ($(($carrier:literal, $cranelift:ident, $llvm:ident)),+ $(,)?) => {$(
        #[test]
        #[ignore = "explicit native integration target"]
        fn $cranelift() { finalizer_entry(Backend::Cranelift, $carrier); }
        #[test]
        #[ignore = "explicit native integration target"]
        fn $llvm() { finalizer_entry(Backend::Llvm, $carrier); }
    )+};
}
finalizer_entries!(
    ("plain", finalizers_plain_cranelift, finalizers_plain_llvm),
    ("list", finalizers_list_cranelift, finalizers_list_llvm),
    ("record", finalizers_record_cranelift, finalizers_record_llvm),
    ("variant", finalizers_variant_cranelift, finalizers_variant_llvm),
    ("option", finalizers_option_cranelift, finalizers_option_llvm),
    ("box", finalizers_box_cranelift, finalizers_box_llvm),
    ("generic_resource", finalizers_generic_resource_cranelift, finalizers_generic_resource_llvm),
    ("generic", finalizers_generic_cranelift, finalizers_generic_llvm),
);

#[test]
fn finalizers_rc_resource_refuses_spec_rule_and_site() {
    // OWN-4's Rc payload example, with a resource in place of its own Note.
    let source = "resource type R { d: u64 }\ntype Holder { r: Rc[R] }\n";
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(source, dir.path());
    request.entry = None;
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let diagnostic = product.machine_diagnostics().unwrap().into_iter().find(|d| d.primary.module == "app@1/main").unwrap();
    assert_eq!(diagnostic.rule, "OWN-4");
    let start = (source.find("Rc[R]").unwrap() + 3) as u32;
    assert_eq!((diagnostic.primary.span.start, diagnostic.primary.span.end), (start, start + 1));
}
