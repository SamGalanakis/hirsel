// The release std (std-reuse-1520 S1). Two laws over the image-backed std:
//
// Independence: the std's parts are a function of the std product. Two
// programs over the same image, each lowering the std cold into its own parts
// store, hold equal record and part bytes. Mutant: the std's roots less the
// definitions a program reaches, red on "the std's record".
//
// Semantic differential: a program assembled over the separately lowered std
// does and reports what the same program assembled whole does: its exit,
// output and failure report, its logical charge report, its failure under a
// root step limit, and how its recorded tape replays. Oracle: today's whole assembly of
// the same publication. Mutant: the std's site points left out of the
// program's source map, red on "what the program does and reports" (fails).

/// A program failing inside a plain std function, whose site only the std's
/// points place.
const STD_FAILS: (&str, &str) = ("fails", "pub fn main() -> Result[(), Exit] {\n    let mut s = string.new()\n    let _ = string.push_uint(mut s, 5, 1u32, 0, ' ')\n    Ok(())\n}\n");

/// One check of `source` as project `project` over std imaged into `cache`.
fn std_checked(dir: &Path, project: &str, source: &str, cache: &str) -> Arc<CheckProduct> {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join(project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join(cache)).unwrap();
    std::fs::write(project.join("main.hirsel"), source).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join(cache), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    product
}

fn std_of(product: &CheckProduct, options: &NativeOptions) -> crate::native::std_release::StdSide {
    let runtime = crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)
        .unwrap()
        .0
        .runtime;
    product.std_side(options, runtime).unwrap()
}

/// What a run of a program shows: exit, stdout, stderr, charge report, tape.
type Shown = (Option<i32>, Vec<u8>, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

/// Assemble `product`'s entry whole, or over its separately lowered std, and
/// run it with a fresh charge report and tape.
fn std_run(product: &CheckProduct, options: &NativeOptions, separate: bool, dir: &Path) -> Shown {
    use crate::native::recipe_build::Publication;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let code = crate::code_store::CodeStore::new(dir.join("code"));
    let reached = product.reachable(&entry, options.instance_limit).unwrap();
    let publication = product
        .publish_release(&entry, &reached, options, &code, 16 * 1024 * 1024)
        .unwrap();
    let std = separate.then(|| std_of(product, options));
    if let Some(std) = &std {
        assert!(!std.roots.is_empty(), "the std holds roots");
    }
    let built = Publication::open(&code, publication.root, &crate::request::local_admission_limits())
        .unwrap()
        .assemble_with(options, reached.counters, std)
        .unwrap();
    if let Some(report) = &options.logical_charge_report {
        let _ = std::fs::remove_file(report);
    }
    let tape = dir.join("run.tape");
    let _ = std::fs::remove_file(&tape);
    let mut command = Command::new(&built.executable);
    command.current_dir(dir).env_remove("HIRSEL_RECORD");
    if options.recording && options.replay.is_none() {
        // A fixed tape id, so the two builds' tapes differ only by build.
        command.env("HIRSEL_RECORD", &tape).env("HIRSEL_RECORD_ID", "00112233445566778899aabbccddeeff");
    }
    let run = command.output().unwrap();
    (
        run.status.code(),
        run.stdout,
        run.stderr,
        options.logical_charge_report.as_ref().map(|p| std::fs::read(p).expect("the run wrote its charge report")),
        // Both builds must agree on whether a run leaves a tape.
        options.recording.then(|| std::fs::read(&tape).ok()).flatten(),
    )
}

#[test]
#[ignore = "explicit native integration target"]
fn a_separate_std_is_the_std_products_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut parts = Vec::new();
    for (name, source) in [UNIT_PROGRAMS[0], UNIT_PROGRAMS[2]] {
        let product = std_checked(dir.path(), name, source, "cache");
        let mut options = native_options(Backend::Llvm, &dir.path().join(name));
        options.optimize = true;
        options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        let std = std_of(&product, &options);
        let shared = Store::new(&options.parts).unwrap();
        let record = shared.get(std.key).unwrap().expect("the std record");
        let bytes: Vec<Vec<u8>> =
            std.objects.iter().map(|object| std::fs::read(&object.path).unwrap()).collect();
        parts.push((std.key, record, bytes));
    }
    assert_eq!(parts[0].0, parts[1].0, "the std key");
    assert!(parts[0].1 == parts[1].1, "the std's record");
    assert!(parts[0].2 == parts[1].2, "the std's parts");
}

/// Each corpus program, assembled whole and over the separate std, under each
/// of `modes` (the options a mode changes from the base); the number of runs
/// whose tape both builds replayed.
fn std_differential(modes: &[&str]) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let runtime = |name: &str| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(std::env::var_os(name).unwrap_or_else(|| panic!("{name}")))
    };
    let mut taped = 0;
    for (name, source) in UNIT_PROGRAMS.into_iter().chain([STD_FAILS]) {
        let product = std_checked(dir.path(), name, source, "cache");
        let project = dir.path().join(name);
        let mut base = native_options(Backend::Llvm, &dir.path().join("shared"));
        base.optimize = true;
        base.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        base.debug_lines = true;
        // Stdout, for the program that takes it.
        base.grant_mask = if source.contains("out: Stdout") { 1 << 4 } else { 0 };
        for &mode in modes {
            let options = match mode {
                "plain" => base.clone(),
                "charges" => NativeOptions {
                    logical_charge_report: Some(project.join("charges")),
                    runtime: runtime("HIRSEL_CORE_RT_OBSERVED_ARCHIVE"),
                    ..base.clone()
                },
                "limited" => NativeOptions { root_limit: 40, ..base.clone() },
                "recorded" => NativeOptions {
                    recording: true,
                    runtime: runtime("HIRSEL_CORE_RT_RECORD_ARCHIVE"),
                    ..base.clone()
                },
                _ => unreachable!("{mode}"),
            };
            let whole = std_run(&product, &options, false, &project);
            let separate = std_run(&product, &options, true, &project);
            assert_eq!(
                (whole.0, &whole.1, &whole.2),
                (separate.0, &separate.1, &separate.2),
                "{name} {mode}: what the program does and reports"
            );
            assert_eq!(whole.3, separate.3, "{name} {mode}: the charge report");
            // A tape names its executable's launch image, so two builds' tapes
            // differ there; each must replay on the other build as on its own.
            assert_eq!(whole.4.is_some(), separate.4.is_some(), "{name} {mode}: a tape");
            for (label, tape) in [("whole", &whole.4), ("separate", &separate.4)] {
                let Some(tape) = tape else { continue };
                taped += 1;
                let path = project.join(format!("{label}.tape"));
                std::fs::write(&path, tape).unwrap();
                let replay = NativeOptions {
                    replay: Some(crate::ReplayPlan {
                        path: path.to_str().unwrap().into(),
                        diagnose: false,
                    }),
                    grant_mask: 0,
                    ..options.clone()
                };
                let on_whole = std_run(&product, &replay, false, &project);
                let on_separate = std_run(&product, &replay, true, &project);
                assert_eq!(
                    (on_whole.0, &on_whole.1, &on_whole.2),
                    (on_separate.0, &on_separate.1, &on_separate.2),
                    "{name}: the {label} build's tape replayed on each build"
                );
                // A faithful replay ends 0 and repeats no live write, even of
                // a failed run (RL1).
                assert_eq!(
                    (on_whole.0, on_whole.1.is_empty()),
                    (Some(0), true),
                    "{name}: the {label} build's tape replays"
                );
            }
        }
    }
    taped
}

#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_does_what_it_does_whole() {
    std_differential(&["plain", "charges", "limited"]);
}

/// Recording opens the tape's directory through the OS Dir provider (openat2),
/// so this body runs on the host.
#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_records_what_it_records_whole() {
    assert!(std_differential(&["recorded"]) > 0, "a recorded run wrote a tape");
}
