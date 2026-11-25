// FORM #98/#100: a callee body changes the caller's facts across a lend; the
// caller's checked record is reused and the program facts are rederived.
fn program_facts_source_changes(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let caller = DefPath::from_full("app@1/main/caller").unwrap();
    let mut options = native_options(backend, dir.path());
    options.optimize = matches!(backend, Backend::Llvm);
    let mut driver = Driver::default();
    let mut caller_key = None;
    let mut consumed = Vec::new();
    for (lend, status) in [
        ("string.truncate(mut s, 1)", 1),
        ("string.push(mut s, \"x\")", 4),
    ] {
        let source = format!("fn caller(mut s: String) {{ lend(mut s) }}\npub fn main() -> Result[(), Exit] {{ let mut s = \"abc\"; caller(mut s); Error(Exit(status: int.truncate(string.byte_len(s)))) }}\nfn lend(mut s: String) {{ {lend} }}\n");
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(
            &root.join("core/core"), package("core"), true,
        ).unwrap();
        let checked = driver.check(request.clone(), &mut Meter::new(false)).unwrap();
        assert!(checked.accepted(), "{:?} {:?}",checked.diagnostics,checked.incomplete);
        let key = checked.demand(&caller).unwrap().key.clone();
        if let Some(previous) = &caller_key {
            // Program facts are derived at the native stage from the reached
            // bodies' identities; the caller's check reads only signatures.
            assert_eq!(&key, previous, "a callee body edit must keep the caller's check key");
        }
        caller_key = Some(key);
        let before = facts_selections(&options.cas);
        let built = checked.build(&entry,&options).unwrap();
        run_clean(&built.executable,status);
        if matches!(backend, Backend::Llvm) {
            // The caller's rows (its body's and its declarations') in what the
            // release emitter consumed for this program.
            let ir = checked.demand(&caller).unwrap().ir().unwrap();
            let f = ir.functions.iter().position(|f| f.body.is_some()).unwrap();
            let code = ir.code_identity(hirsel_core_ir::FuncId(f as u32));
            // A selection holds the address it selects, then its own name.
            let fresh: Vec<_> = facts_selections(&options.cas).difference(&before).filter_map(|path| {
                let bytes = std::fs::read(path).ok()?;
                (bytes.get(32..38) == Some(b"facts-")).then(|| ContentHash(bytes[..32].try_into().unwrap()))
            }).collect();
            let [address] = fresh.as_slice() else { panic!("one fresh facts selection, got {fresh:?}") };
            let store = Store::cas(&options.cas).unwrap();
            let bytes = store.get(*address).unwrap().unwrap();
            let rows = decode::<Vec<Vec<hirsel_core_ir::roles::BodyFacts>>>(&bytes).unwrap();
            let caller_rows: Vec<_> = rows.concat().into_iter().filter(|r| r.code == code).map(|r| r.params).collect();
            assert!(!caller_rows.is_empty(), "the caller is in the consumed rows");
            consumed.push(caller_rows);
        }
        let warm = Driver::default().check(request,&mut Meter::new(false)).unwrap();
        assert!(warm.rechecked.is_empty());
        let built = warm.build(&entry,&options).unwrap();
        assert_eq!(built.compiled,0);
        assert!(!built.linked);
        run_clean(&built.executable,status);
    }
    if let [truncate, push] = consumed.as_slice() {
        // The two lend bodies give s different facts; the caller's body is
        // unchanged, and its consumed rows must take the callee's change.
        assert_ne!(truncate, push, "a callee's fact change must reach its unchanged caller's consumed rows");
    }
}
fn facts_selections(store: &Path) -> std::collections::BTreeSet<std::path::PathBuf> {
    crate::store::collect::records(store).into_iter().map(|(path, _)| path).collect()
}
#[test]
#[ignore = "explicit native integration target"]
fn program_facts_lend_change_cranelift() { program_facts_source_changes(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn program_facts_lend_change_llvm() { program_facts_source_changes(Backend::Llvm); }
