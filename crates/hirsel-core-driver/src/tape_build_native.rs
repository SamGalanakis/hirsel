// REPLAY-SPEC v2.1 R1: every executable carries its key as the 32-byte
// symbol `hirsel_build_identity`, which a tape's header records and a Replay
// open compares as the revision (v2.3 R10; the runtime's
// open_refuses_a_changed_code_revision_arg_limit_or_policy).

/// The symbol's bytes, read from the executable's own symbol table.
fn build_identity_of(executable: &Path) -> [u8; 32] {
    use object::{Object, ObjectSection, ObjectSymbol};
    let bytes = std::fs::read(executable).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    let symbol = file
        .symbols()
        .find(|s| s.name() == Ok("hirsel_build_identity"))
        .expect("the executable defines hirsel_build_identity");
    let section = file
        .section_by_index(symbol.section_index().unwrap())
        .unwrap();
    let at = (symbol.address() - section.address()) as usize;
    section.data().unwrap()[at..at + 32].try_into().unwrap()
}

/// Wrong implementation caught: a symbol that is not the executable's key
/// (a constant, the sources digest alone, the entry plan's), so a rebuild
/// with a changed reachable body would replay an old tape silently, or an
/// unchanged rebuild would refuse its own tape.
fn build_identity_is_the_key(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let build = |text: &str, store: &str| {
        let source = format!(
            "pub fn main(output: Stdout) -> Result[(), Exit] waits {{\n    result.unwrap(stdout.write(output, \"{text}\"))\n    Ok(())\n}}"
        );
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        let mut options = native_options(backend, dir.path());
        options.store = dir.path().join(store);
        options.output = dir.path().join(format!("{store}-{text}"));
        let built = product.build(&entry, &options).unwrap();
        (built.identity, build_identity_of(&built.executable))
    };
    let (key, first) = build("entered", "a");
    assert_eq!(first, key.0, "the symbol is the executable's key");
    let (_, rebuilt) = build("entered", "b");
    assert_eq!(rebuilt, first, "an unchanged rebuild in a fresh store keeps it");
    // One byte of main's body; main's identity (its signature) is unchanged.
    let (_, changed) = build("enterEd", "a");
    assert_ne!(changed, first, "a changed reachable body changes it");
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_build_identity_is_the_key_cranelift() {
    build_identity_is_the_key(Backend::Cranelift);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_build_identity_is_the_key_llvm() {
    build_identity_is_the_key(Backend::Llvm);
}
