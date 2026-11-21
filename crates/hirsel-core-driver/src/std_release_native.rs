// The release std (std-reuse-1520 S1, S2). Two laws over the image-backed std:
//
// Independence: the std's parts, and the bodies it lends, are a function of
// the std product. The std product's own prebuild, and a program's build over
// the same image lowering the std cold into its own parts store, hold equal
// record and part bytes. Mutant: the
// std's roots less the definitions a program reaches, red on "the std's record".
//
// Semantic differential: a program assembled over the separately lowered std,
// calling it out of line (Parts) or importing and inlining its small bodies
// (Imported), does and reports what the same program assembled whole does: its
// exit, output and failure report, its logical charge report, its failure
// under a root step limit, and how its recorded tape replays. Oracle: the
// out-of-line assembly over the same std parts. Mutants: the std's site points
// left out of the program's source map, and imported bodies whose site rows
// are rebased to the program, each red on "fails plain Imported: what the
// program does and reports"; imported bodies that charge one step more than
// their parts, red on the same assertion (the printed step thresholds).

/// A program over small plain std bodies the program imports and inlines (the
/// std part holds each out of line): it prints the fewest steps under which three
/// std calls finish, which the bodies' step charges fix, then fails inside a
/// std function, whose site only the std's points place.
const STD_FAILS: (&str, &str) = (
    "fails",
    r#"pub fn main(out: Stdout) -> Result[(), Exit] {
    let s = "the quick brown fox jumps over the lazy dog"
    let mut n = 0u64
    let mut done = false
    while !done {
        let found = bound steps n {
            string.find(s, 0, "dog")
        }
        match found {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                n += 1
            }
        }
    }
    let mut m = 0u64
    done = false
    while !done {
        let parsed = bound steps m {
            int.parse_uint("123456789012345678", 10u32)
        }
        match parsed {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                m += 1
            }
        }
    }
    let mut k = 0u64
    done = false
    while !done {
        let finite = bound steps k {
            float.is_finite(float.from_bits64(k))
        }
        match finite {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                k += 1
            }
        }
    }
    let mut line = string.new()
    let _ = string.push_uint(mut line, n, 10u32, 0, ' ')
    let _ = string.push_uint(mut line, m, 10u32, 1, ' ')
    let _ = string.push_uint(mut line, k, 10u32, 1, ' ')
    result.unwrap(stdout.write(out, line));
    let _ = char.digit('a', 1u32)
    Ok(())
}
"#,
);

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

/// How a program's build takes the std.
#[derive(Clone, Copy, Debug)]
enum StdBuild {
    /// Composed with the program.
    Whole,
    /// Over the std's parts alone: each call reaches the out-of-line part.
    Parts,
    /// Over the std's parts, with its lent bodies imported for inlining.
    Imported,
}

/// Assemble `product`'s entry as `build` takes the std, and run it with a
/// fresh charge report and tape.
fn std_run(product: &CheckProduct, options: &NativeOptions, build: StdBuild, dir: &Path) -> Shown {
    use crate::native::recipe_build::Publication;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let code = crate::code_store::CodeStore::new(dir.join("code"));
    let reached = product.reachable(&entry, options.instance_limit).unwrap();
    let publication = product
        .publish_release(&entry, &reached, options, &code, 16 * 1024 * 1024)
        .unwrap();
    let std = match build {
        StdBuild::Whole => None,
        StdBuild::Parts => Some(crate::native::std_release::StdSide { lent: Vec::new(), ..std_of(product, options) }),
        StdBuild::Imported => Some(std_of(product, options)),
    };
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
    // The first program's std comes from the std product's own prebuild in
    // its image; the second's image holds none, so its build lowers the std.
    for ((name, source), cache) in [(UNIT_PROGRAMS[0], "cache"), (UNIT_PROGRAMS[2], "cache-b")] {
        let product = std_checked(dir.path(), name, source, cache);
        let mut options = native_options(Backend::Llvm, &dir.path().join(name));
        options.optimize = true;
        options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        let image = product.image.as_ref().unwrap().store_root().join("release-std");
        if cache == "cache" {
            let runtime = crate::tool_inputs::runtime_identity(hirsel_core_ids::ContentHash::of_bytes(
                &std::fs::read(&options.runtime).unwrap(),
            ));
            product.prebuild_release_std(&options.target, runtime).unwrap();
        }
        let std = std_of(&product, &options);
        let store = Store::new(if cache == "cache" { &image } else { &options.parts }).unwrap();
        let record = store.get(std.key).unwrap().expect("the std record");
        let bytes: Vec<Vec<u8>> =
            std.objects.iter().map(|object| std::fs::read(&object.path).unwrap()).collect();
        assert!(!std.lent.is_empty(), "the std lends bodies");
        parts.push((std.key, record, bytes, std.lent));
    }
    assert_eq!(parts[0].0, parts[1].0, "the std key");
    assert!(parts[0].1 == parts[1].1, "the std's record");
    assert!(parts[0].2 == parts[1].2, "the std's parts");
    assert!(parts[0].3 == parts[1].3, "the std's lent bodies");
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
                // The release unit, whose failures name their site rows.
                "release" => NativeOptions { debug_lines: false, ..base.clone() },
                "recorded" => NativeOptions {
                    recording: true,
                    runtime: runtime("HIRSEL_CORE_RT_RECORD_ARCHIVE"),
                    ..base.clone()
                },
                _ => unreachable!("{mode}"),
            };
            // The out-of-line parts are the oracle: a composed build, and an
            // inlined copy of a std body, does and reports what its part does.
            let parts = std_run(&product, &options, StdBuild::Parts, &project);
            let builds = [StdBuild::Parts, StdBuild::Whole, StdBuild::Imported];
            let mut tapes = vec![(StdBuild::Parts, parts.4.clone())];
            for build in [StdBuild::Whole, StdBuild::Imported] {
                let shown = std_run(&product, &options, build, &project);
                assert_eq!(
                    (parts.0, &parts.1, &parts.2),
                    (shown.0, &shown.1, &shown.2),
                    "{name} {mode} {build:?}: what the program does and reports"
                );
                assert_eq!(parts.3, shown.3, "{name} {mode} {build:?}: the charge report");
                assert_eq!(parts.4.is_some(), shown.4.is_some(), "{name} {mode} {build:?}: a tape");
                tapes.push((build, shown.4));
            }
            // A tape names its executable's launch image, so two builds' tapes
            // differ there; each must replay on every build as on its own.
            for (label, tape) in &tapes {
                let Some(tape) = tape else { continue };
                taped += 1;
                let path = project.join(format!("{label:?}.tape"));
                std::fs::write(&path, tape).unwrap();
                let replay = NativeOptions {
                    replay: Some(crate::ReplayPlan {
                        path: path.to_str().unwrap().into(),
                        diagnose: false,
                    }),
                    grant_mask: 0,
                    ..options.clone()
                };
                let on_parts = std_run(&product, &replay, StdBuild::Parts, &project);
                for build in builds {
                    let on = std_run(&product, &replay, build, &project);
                    assert_eq!(
                        (on_parts.0, &on_parts.1, &on_parts.2),
                        (on.0, &on.1, &on.2),
                        "{name}: the {label:?} build's tape replayed on {build:?}"
                    );
                }
                // A faithful replay ends 0 and repeats no live write, even of
                // a failed run (RL1).
                assert_eq!(
                    (on_parts.0, on_parts.1.is_empty()),
                    (Some(0), true),
                    "{name}: the {label:?} build's tape replays"
                );
            }
        }
    }
    taped
}

#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_does_what_it_does_whole() {
    std_differential(&["plain", "release", "charges", "limited"]);
}

/// Recording opens the tape's directory through the OS Dir provider (openat2),
/// so this body runs on the host.
#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_records_what_it_records_whole() {
    assert!(std_differential(&["recorded"]) > 0, "a recorded run wrote a tape");
}
