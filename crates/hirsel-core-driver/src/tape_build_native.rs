// REPLAY-SPEC v2.3 R10: every executable carries its entry's code revision as
// the 32-byte symbol `hirsel_code_revision`, which a Run tape's header records
// and a Replay open compares (the runtime's
// open_refuses_a_changed_code_revision_arg_limit_or_policy).

/// The symbol's bytes, read from the executable's own symbol table.
fn code_revision_of(executable: &Path) -> [u8; 32] {
    use object::{Object, ObjectSection, ObjectSymbol};
    let bytes = std::fs::read(executable).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    let symbol = file
        .symbols()
        .find(|s| s.name() == Ok("hirsel_code_revision"))
        .expect("the executable defines hirsel_code_revision");
    let section = file
        .section_by_index(symbol.section_index().unwrap())
        .unwrap();
    let at = (symbol.address() - section.address()) as usize;
    section.data().unwrap()[at..at + 32].try_into().unwrap()
}

/// The oracle is the revision the build reports (revision.rs, from the checked
/// IR, not from the link), which a Call header must also agree with. Wrong
/// implementations caught: a symbol that is the executable's key, the sources
/// digest (a comment edit would refuse) or any other hash than the revision.
#[test]
#[ignore = "explicit native integration target"]
fn tape_code_revision_is_the_entry_revision() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = "pub fn main(output: Stdout) -> Result[(), Exit] waits {\n    result.unwrap(stdout.write(output, \"entered\"))\n    Ok(())\n}";
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        let mut options = native_options(backend, dir.path());
        options.output = dir.path().join(format!("{backend:?}"));
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(code_revision_of(&built.executable), built.revision.0, "{backend:?}");
    }
}

/// Builds core/front/tape/run.hirsel, with `edit` applied to its text,
/// into `store`; the run's directory grant is `grant`.
fn tape_run_build(backend: Backend, dir: &Path, store: &str, edit: (&str, &str)) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = std::fs::read_to_string(root.join("core/front/tape/run.hirsel"))
        .unwrap()
        .replace(edit.0, edit.1);
    let mut request = source_request(&source, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let mut options = native_options(backend, dir);
    options.store = dir.join(store);
    options.output = dir.join(format!("{store}-program"));
    options.directories = vec![dir.join("grant").to_str().unwrap().into()];
    options.grant_mask = (1 << 2) | (1 << 4) | (1 << 9);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    product.build(&entry, &options).unwrap().executable
}

fn tape_run(program: &Path, argv: &[&str]) -> (Option<i32>, String, String) {
    let output = std::process::Command::new(program)
        .args(argv)
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// REPLAY-SPEC v2.3 §9 end to end, the oracle being the recording run's own
/// output: a replay prints the recorded clock reading and argument vector, and
/// nothing through the substitute Stdout. Wrong implementations caught: a
/// replayed substitute that reaches the host (a second "taped", a fresh clock
/// reading), Args answered from the live vector or not readable after end
/// ("|two"), a Replay that writes the tape, END's outcome not compared (exit 5);
/// a revision that changes with the build (a rebuild with the other engine
/// refuses, exit 3); a changed body that replays without Across, an
/// Across that refuses it or does not say so ("across:"), or says so of its own
/// revision; an Across replay whose changed outcome is not Diverged(Outcome).
fn tape_run_replays(backend: Backend, other: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let grant = dir.path().join("grant");
    std::fs::create_dir(&grant).unwrap();
    let program = tape_run_build(backend, dir.path(), "a", ("", ""));
    let (code, recorded, err) = tape_run(&program, &["one"]);
    assert_eq!(code, Some(0), "record: {err}");
    let reading = recorded
        .strip_prefix("taped")
        .and_then(|s| s.strip_suffix("|one"))
        .unwrap_or_else(|| panic!("record printed {recorded:?}"));
    assert!(reading.parse::<u64>().is_ok(), "{recorded:?}");
    let tape = std::fs::read(grant.join("run.tape")).unwrap();
    let (code, replayed, err) = tape_run(&program, &["two"]);
    assert_eq!(code, Some(0), "replay: {err}");
    assert_eq!(replayed, format!("{reading}|one"), "replay answers from the tape");
    assert_eq!(std::fs::read(grant.join("run.tape")).unwrap(), tape);
    let (code, _, err) = tape_run(&program, &["x"]);
    assert_eq!(code, Some(5), "a changed outcome is Diverged(Outcome): {err}");
    let (code, forced, err) = tape_run(&program, &["force"]);
    assert_eq!((code, forced), (Some(0), replayed.clone()), "Across of its own revision: {err}");
    let switched = tape_run_build(other, dir.path(), "e", ("", ""));
    let (code, again, err) = tape_run(&switched, &["three"]);
    assert_eq!((code, again), (Some(0), replayed), "the other engine: {err}");
    let changed = tape_run_build(backend, dir.path(), "c", ("\"|\"", "\"/\""));
    let (code, printed, err) = tape_run(&changed, &["one"]);
    assert_eq!((code, printed), (Some(3), String::new()), "Refused(Revision): {err}");
    let (code, printed, err) = tape_run(&changed, &["force"]);
    assert_eq!(
        (code, printed),
        (Some(0), format!("across:{reading}/one")),
        "Across opens it, labelled: {err}"
    );
    let behaves = tape_run_build(backend, dir.path(), "d", ("{ 1u8 } else { 0u8 }", "{ 1u8 } else { 6u8 }"));
    let (code, _, err) = tape_run(&behaves, &["force"]);
    assert_eq!(code, Some(5), "a changed outcome under Across is Diverged(Outcome): {err}");
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_cranelift() {
    tape_run_replays(Backend::Cranelift, Backend::Llvm);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_llvm() {
    tape_run_replays(Backend::Llvm, Backend::Cranelift);
}

/// A substitute used after `tape.end` fails with cause `Tape`, printed from
/// the runtime's verdict (D-B4), in Record and in Replay. Wrong
/// implementations caught: a substitute that keeps reaching the host after
/// end (exit 0), a bare `Tape` cause with no verdict, a refusal with the
/// wrong cause kind.
fn tape_run_after_end(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("grant")).unwrap();
    let program = tape_run_build(backend, dir.path(), "a", ("", ""));
    for run in ["record", "replay"] {
        let (code, out, err) = tape_run(&program, &["after"]);
        assert_ne!(code, Some(0), "{run}: {out:?}");
        assert!(err.starts_with("Tape(ended) at "), "{run}: {err:?}");
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_after_end_cranelift() {
    tape_run_after_end(Backend::Cranelift);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_after_end_llvm() {
    tape_run_after_end(Backend::Llvm);
}
