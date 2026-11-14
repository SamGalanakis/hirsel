// FORM #98/#100: a callee body changes the caller's facts across a lend.
fn program_facts_source_changes(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let caller = DefPath::from_full("app@1/main/caller").unwrap();
    let mut options = native_options(backend, dir.path());
    options.optimize = matches!(backend, Backend::Llvm);
    let mut driver = Driver::default();
    let mut caller_key = None;
    for (lend, status) in [
        ("string.truncate(mut s, 1)", 1),
        ("result.unwrap(string.push(mut s, \"x\"))", 4),
    ] {
        let source = format!("fn caller(mut s: String) {{ lend(mut s) }}\npub fn main() -> Result[(), Exit] {{ let mut s = \"abc\"; caller(mut s); Error(Exit(status: int.truncate(string.byte_len(s)))) }}\nfn lend(mut s: String) {{ {lend} }}\n");
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(
            &root.join("core/core"), package("core"), true,
        ).unwrap();
        let checked = driver.check(request.clone(), &mut Meter::new(false)).unwrap();
        assert!(checked.accepted(), "{:?} {:?}",checked.diagnostics,checked.incomplete);
        let key = checked.demand(&caller).unwrap().key.clone();
        if let Some(previous) = &caller_key { assert_eq!(&key, previous); }
        caller_key = Some(key);
        let built = checked.build(&entry,&options).unwrap();
        run_clean(&built.executable,status);
        let warm = Driver::default().check(request,&mut Meter::new(false)).unwrap();
        assert!(warm.rechecked.is_empty());
        let built = warm.build(&entry,&options).unwrap();
        assert_eq!(built.compiled,0);
        assert!(!built.linked);
        run_clean(&built.executable,status);
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn program_facts_lend_change_cranelift() { program_facts_source_changes(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn program_facts_lend_change_llvm() { program_facts_source_changes(Backend::Llvm); }
