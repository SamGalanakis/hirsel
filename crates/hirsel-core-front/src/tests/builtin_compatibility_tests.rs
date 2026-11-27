use super::*;

#[test]
fn incompatible_core_builtin_signature_is_refused() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    for file in std::fs::read_dir(core_dir()).unwrap() {
        let file = file.unwrap();
        if file.path().is_file() && file.file_name() != "list.hirsel" {
            std::fs::copy(file.path(), dir.join(file.file_name())).unwrap();
        }
    }
    let list = std::fs::read_to_string(core_dir().join("list.hirsel")).unwrap();
    let incompatible = list
        .replace("pub fn new[T]() -> List[T]", "pub fn new() -> List[u64]")
        .replace(
            "pub fn slice[T](xs: List[T], lo: u64, hi: u64)",
            "pub fn slice[T](xs: List[T])",
        );
    std::fs::write(dir.join("list.hirsel"), incompatible).unwrap();
    let (index, inputs) = inputs_with_core(
        "fn new() -> List[u64] { list.new() }\nfn slice(xs: List[u64]) { let _ = list.slice(xs) }",
        dir,
    );
    let p = Program::declare(&index, &inputs);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    for (name, declared, required) in [
        (
            "new",
            "0 type arguments, 0 value arguments",
            "1 type arguments, 0 value arguments",
        ),
        (
            "slice",
            "1 type arguments, 1 value arguments",
            "1 type arguments, 3 value arguments",
        ),
    ] {
        let f = p
            .fns
            .iter()
            .position(|fd| fd.path.package_is("app", "1") && fd.path.0.name == name)
            .unwrap();
        let check = p.check_lower(f, &mut Emitter::new());
        let [refusal] = check.refusals.as_slice() else {
            panic!("{check:?}");
        };
        assert_eq!(refusal.rule, "LANG-8");
        assert!(
            refusal.message.contains("incompatible core sources"),
            "{refusal:?}"
        );
        assert!(
            refusal.message.contains(&format!("core@1/list/{name}")),
            "{refusal:?}"
        );
        assert_eq!(refusal.declared.as_deref(), Some(declared));
        assert_eq!(refusal.required.as_deref(), Some(required));
        assert!(check.gaps.is_empty(), "{check:?}");
    }
}
