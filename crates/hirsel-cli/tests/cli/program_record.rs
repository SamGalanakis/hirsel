//! Program record B end to end. Each answer from a record is compared with an
//! independently forced cold run: the same command under a state home that
//! holds no registration, so no record can answer it. A record must answer
//! exactly when every input is unchanged, and its answer must equal cold.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Variables the toolchain reads for its own inputs; every other `HIRSEL_*`
/// from the test environment is removed so it cannot bypass the record.
const PASSED: &[&str] = &[
    "HIRSEL_RUNTIME_ARCHIVES",
    "HIRSEL_LINK_KIT",
    "HIRSEL_CAPTURE_LINKER",
    "HIRSEL_AUDITOR_PACKAGE",
    "HIRSEL_GLIBC_FLOOR",
];

struct Home {
    root: tempfile::TempDir,
}

#[derive(Debug, PartialEq)]
struct Answer {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, PartialEq)]
struct Counts {
    hits: u64,
    misses: u64,
    publications: u64,
    /// Why the run did not use records, as the timings report names it.
    bypass: Option<String>,
}

/// A run that did not use records, for `reason`.
fn bypassed(reason: &str) -> Counts {
    Counts {
        hits: 0,
        misses: 0,
        publications: 0,
        bypass: Some(reason.to_owned()),
    }
}

impl Home {
    /// A private home whose state holds the registration `precompute` writes
    /// for its products.
    fn new() -> Self {
        let home = Self {
            root: tempfile::tempdir().unwrap(),
        };
        home.register("pub fn unused()->u8{7u8}\n");
        home
    }
    fn path(&self) -> &Path {
        self.root.path()
    }
    fn state(&self) -> PathBuf {
        self.path().join("state")
    }
    /// (Re)write the installed products, which replaces the registration.
    fn register(&self, library: &str) {
        let lib = self.path().join("reg");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("library.hir"), library).unwrap();
        std::fs::write(lib.join("hirsel.json"), r#"{"schema":1,"package":{"name":"reg","version":"1"},"sources":[{"path":"library.hir","module":[]}],"dependencies":[]}"#).unwrap();
        let _ = std::fs::remove_dir_all(self.path().join("products"));
        let output = self
            .command(&self.state())
            .arg("precompute")
            .arg(lib.join("hirsel.json"))
            .arg("-o")
            .arg(self.path().join("products"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    fn command(&self, state: &Path) -> Command {
        // Absolute: Buck2 names the binary relative to the test's own directory.
        let mut command = Command::new(std::path::absolute(env!("CARGO_BIN_EXE_hirsel")).unwrap());
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("HIRSEL_") && !PASSED.contains(&name.as_ref()) {
                command.env_remove(name.as_ref());
            }
        }
        command
            .env("XDG_STATE_HOME", state)
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("XDG_DATA_HOME", self.path().join("data"))
            .env("HIRSEL_PRODUCTS", self.path().join("products"))
            .env("HIRSEL_DAEMON", "off");
        command
    }
    /// One run with its record counts.
    fn run(
        &self,
        state: &Path,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (Answer, Counts) {
        let (answer, counts, _) = self.report(state, cwd, args, env);
        (answer, counts)
    }
    /// One run with its record counts and the number of full platform
    /// captures it ran (the platform-record misses in its timings report).
    fn report(
        &self,
        state: &Path,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (Answer, Counts, u64) {
        let mut command = self.command(state);
        command.current_dir(cwd).arg("--timings=json").args(args);
        for (name, value) in env {
            match *value {
                UNSET => command.env_remove(name),
                value => command.env(name, value),
            };
        }
        let output: Output = command.output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let trimmed = stderr.trim_end_matches('\n');
        let (rest, report) = trimmed.rsplit_once('\n').unwrap_or(("", trimmed));
        // A process stopped by its guard prints no report and recorded nothing.
        let (rest, report) = match serde_json::from_str::<serde_json::Value>(report) {
            Ok(report) => (rest.to_owned(), report),
            Err(_) => (
                trimmed.to_owned(),
                serde_json::json!({"counts": {
                "program record hits": 0, "program record misses": 0, "program record publications": 0}}),
            ),
        };
        let count = |name: &str| report["counts"][name].as_u64().unwrap();
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!("{rest}\n")
        };
        let captures = report["runner"]["platform_records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|record| {
                record["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("miss"))
            })
            .map(|record| record["count"].as_u64().unwrap())
            .sum();
        (
            Answer {
                status: output.status.code(),
                stdout: output.stdout,
                stderr: rest.into_bytes(),
            },
            Counts {
                hits: count("program record hits"),
                misses: count("program record misses"),
                publications: count("program record publications"),
                bypass: report["program_record_bypass"].as_str().map(str::to_owned),
            },
            captures,
        )
    }
    fn warm(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (Answer, Counts) {
        self.run(&self.state(), cwd, args, env)
    }
    /// The forced cold oracle: a state home with no registration.
    fn cold(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Answer {
        let state = tempfile::tempdir().unwrap();
        let (answer, counts) = self.run(state.path(), cwd, args, env);
        assert_eq!(
            counts,
            bypassed("no registration"),
            "the cold oracle: {answer:?}"
        );
        answer
    }
}

/// An `env` value that removes the variable instead.
const UNSET: &str = "\0unset";

const MISS: Counts = Counts {
    hits: 0,
    misses: 1,
    publications: 1,
    bypass: None,
};
const HIT: Counts = Counts {
    hits: 1,
    misses: 0,
    publications: 0,
    bypass: None,
};

fn app(root: &Path) -> PathBuf {
    let app = root.join("app");
    let dep = root.join("dep");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(dep.join("dep.hir"), "pub fn value() -> u64 {\n  1u64\n}\n").unwrap();
    std::fs::write(dep.join("hirsel.json"), r#"{"schema":1,"package":{"name":"dep","version":"1"},"sources":[{"path":"dep.hir","module":["dep"]}],"dependencies":[]}"#).unwrap();
    std::fs::write(
        app.join("util.hir"),
        "pub fn twice(x: u64) -> u64 {\n  x + x\n}\n",
    )
    .unwrap();
    std::fs::write(
        app.join("main.hir"),
        "pub fn main() -> Result[(), ()] {\n  failure.check(util.twice(dep.value()) == 2u64, Arithmetic())\n  Ok(())\n}\npub fn other() -> Result[(), ()] {\n  Ok(())\n}\n",
    )
    .unwrap();
    write_manifest(&app, "main", &["util"], "1");
    app
}

fn write_manifest(app: &Path, entry: &str, util: &[&str], dep_version: &str) {
    std::fs::write(
        app.join("hirsel.json"),
        serde_json::json!({"schema":1,"package":{"name":"app","version":"1"},
            "entry":{"module":[],"name":entry},
            "sources":[{"path":"main.hir","module":[]},{"path":"util.hir","module":util}],
            "dependencies":[{"manifest":"../dep/hirsel.json","package":{"name":"dep","version":dep_version},"expose":[]}]})
        .to_string(),
    )
    .unwrap();
}

fn edit(path: &Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from:?} not in {}", path.display());
    std::fs::write(path, text.replacen(from, to, 1)).unwrap();
}

/// An input change the record must not answer across: after it, the first
/// run misses and publishes, the next hits, and both equal cold.
fn flips_then_hits(home: &Home, cwd: &Path, args: &[&str], env: &[(&str, &str)], what: &str) {
    let cold = home.cold(cwd, args, env);
    let (first, counts) = home.warm(cwd, args, env);
    assert_eq!(counts, MISS, "{what}: first run after the change");
    assert_eq!(first, cold, "{what}: miss");
    let (second, counts) = home.warm(cwd, args, env);
    assert_eq!(counts, HIT, "{what}: unchanged rerun");
    assert_eq!(second, cold, "{what}: hit");
}

/// Bug caught: a key P that leaves out an input (a byte, a comment or newline
/// that moves diagnostic positions, the entry, a module mapping, a dependency
/// or its parameter, an option, a key or owner variable, the checkout path),
/// so a stale record answers; or a replay that differs from cold.
#[test]
fn a_check_record_answers_only_unchanged_inputs_and_replays_cold() {
    let home = Home::new();
    let root = home.path();
    let app = app(root);
    let check = ["check", "app/hirsel.json"];
    flips_then_hits(&home, root, &check, &[], "first check");
    // Acquisition and telemetry never key the record.
    let (answer, counts) = home.warm(root, &check, &[("HIRSEL_JOBS", "1")]);
    assert_eq!(
        (answer, counts),
        (home.cold(root, &check, &[]), HIT),
        "jobs 1"
    );
    let (_, counts) = home.warm(root, &check, &[("HIRSEL_JOBS", "2")]);
    assert_eq!(counts, HIT, "jobs 2");
    // A failing check replays cold's diagnostics and status, positions included.
    edit(&app.join("util.hir"), "x + x", "x + true");
    flips_then_hits(&home, root, &check, &[], "body byte (rejected)");
    edit(
        &app.join("util.hir"),
        "pub fn twice",
        "// a comment\npub fn twice",
    );
    flips_then_hits(&home, root, &check, &[], "comment");
    edit(&app.join("util.hir"), "pub fn twice", "\npub fn twice");
    flips_then_hits(&home, root, &check, &[], "newline");
    // A diagnostic in another package, whose logical path (its package's)
    // differs from the physical path a diagnostic cites.
    edit(&root.join("dep/dep.hir"), "  1u64", "  1u64 + true");
    flips_then_hits(&home, root, &check, &[], "dependency diagnostic");
    // Display is not an input: every renderer of `check` (plain, `--json`;
    // `--safe` runs its own analysis outside the record, `--verbose` prints
    // nothing for check) is answered by the same record, as cold renders it.
    let json = ["check", "--json", "app/hirsel.json"];
    for display in [&check[..], &json[..]] {
        let cold = home.cold(root, display, &[]);
        let rendered = String::from_utf8_lossy(&cold.stderr).into_owned()
            + &String::from_utf8_lossy(&cold.stdout);
        assert!(
            rendered.contains("app/util.hir") && rendered.contains("dep/dep.hir"),
            "{display:?}: {rendered}"
        );
        let (answer, counts) = home.warm(root, display, &[]);
        assert_eq!((answer, counts), (cold, HIT), "{display:?}");
    }
    let plain = home.cold(root, &check, &[]);
    assert!(
        plain.stderr.split(|b| *b == b'\n').count() > 4,
        "multi-line diagnostics"
    );
    edit(&root.join("dep/dep.hir"), "  1u64 + true", "  1u64");
    edit(&app.join("util.hir"), "x + true", "x + x");
    flips_then_hits(&home, root, &check, &[], "body byte (accepted)");
    write_manifest(&app, "other", &["util"], "1");
    flips_then_hits(&home, root, &check, &[], "manifest entry");
    write_manifest(&app, "other", &["helpers"], "1");
    flips_then_hits(&home, root, &check, &[], "module mapping");
    edit(
        &root.join("dep/hirsel.json"),
        r#""version":"1""#,
        r#""version":"2""#,
    );
    write_manifest(&app, "other", &["util"], "2");
    flips_then_hits(&home, root, &check, &[], "dependency parameter");
    edit(&root.join("dep/dep.hir"), "1u64", "2u64");
    flips_then_hits(&home, root, &check, &[], "dependency body");
    flips_then_hits(&home, root, &check, &[("LANG", "C.UTF-8")], "key variable");
    let results = root.join("results");
    let results = results.to_str().unwrap();
    flips_then_hits(
        &home,
        root,
        &check,
        &[("HIRSEL_RESULTS_DIR", results)],
        "owner variable",
    );
    // Another checkout of the same bytes is another key: its paths are cited.
    let copy = root.join("moved");
    std::fs::create_dir_all(copy.join("app")).unwrap();
    std::fs::create_dir_all(copy.join("dep")).unwrap();
    for file in [
        "app/main.hir",
        "app/util.hir",
        "app/hirsel.json",
        "dep/dep.hir",
        "dep/hirsel.json",
    ] {
        std::fs::copy(root.join(file), copy.join(file)).unwrap();
    }
    flips_then_hits(&home, &copy, &check, &[], "checkout path");
    // A verification mode runs fresh and leaves the record alone.
    let (_, counts) = home.warm(root, &check, &[("HIRSEL_VERIFY_CACHE", "1")]);
    assert_eq!(
        counts,
        bypassed("variable HIRSEL_VERIFY_CACHE is a bypass mode")
    );
    // A variable the inventory does not classify bypasses, and says so.
    let unclassified = [
        ("HIRSEL_UNCLASSIFIED", "1"),
        ("HIRSEL_PROGRAM_RECORD_DEBUG", "1"),
    ];
    let (answer, counts) = home.warm(root, &check, &unclassified);
    let reason = "variable HIRSEL_UNCLASSIFIED is not in the inventory";
    assert_eq!(counts, bypassed(reason));
    assert_eq!(
        String::from_utf8(answer.stderr).unwrap(),
        format!("hirsel: program record bypassed: {reason}\n")
    );
    // A host-only variable is not an input: the record answers.
    home.warm(root, &check, &[]);
    let (_, counts) = home.warm(root, &check, &[("HIRSEL_GATE_SLOTS", "/elsewhere:9")]);
    assert_eq!(counts, HIT, "host-only variable");
}

/// Bug caught: a record answered without the registration that published it:
/// removed, replaced by a new installation, or a records directory carried
/// into another registration's domain.
#[test]
fn only_the_publishing_registration_answers_a_record() {
    let home = Home::new();
    let root = home.path();
    app(root);
    let check = ["check", "app/hirsel.json"];
    flips_then_hits(&home, root, &check, &[], "first check");
    let verifier = home.state().join("hirsel/verifier");
    let registration = std::fs::read_dir(&verifier)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|e| e == "registration"))
        .unwrap();
    let aside = registration.with_extension("aside");
    std::fs::rename(&registration, &aside).unwrap();
    let (answer, counts) = home.warm(root, &check, &[]);
    assert_eq!(counts, bypassed("no registration"), "registration removed");
    assert_eq!(answer, home.cold(root, &check, &[]), "registration removed");
    std::fs::rename(&aside, &registration).unwrap();
    let (_, counts) = home.warm(root, &check, &[]);
    assert_eq!(counts, HIT, "registration restored");
    // A new installation registers a new domain; the old records stay in theirs.
    let domains = |verifier: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(verifier)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.join("programs").is_dir())
            .collect()
    };
    let old = domains(&verifier);
    home.register("pub fn unused()->u8{8u8}\n");
    flips_then_hits(&home, root, &check, &[], "registration replaced");
    // Carry the old domain's records over the new one's: none answers.
    for domain in domains(&verifier) {
        if old.contains(&domain) {
            continue;
        }
        let programs = domain.join("programs");
        std::fs::remove_dir_all(&programs).unwrap();
        copy_tree(&old[0].join("programs"), &programs);
    }
    let (answer, counts) = home.warm(root, &check, &[]);
    // The carried directory is not this registration's domain: it is refused
    // when opened, and the run says so.
    assert_eq!(counts.hits, 0, "copied records");
    assert!(
        counts
            .bypass
            .as_deref()
            .is_some_and(|reason| reason.starts_with("record domain:")),
        "copied records: {counts:?}"
    );
    assert_eq!(answer, home.cold(root, &check, &[]), "copied records");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Bug caught: a build hit that writes other bytes than the build recorded,
/// leaves a damaged, deleted or non-executable output in place, answers from
/// a check record or a check from a build record, keys a failing build by
/// anything but its sources, or records a failure that was not the source's.
#[test]
fn a_build_record_restores_its_executable_and_replays_failures() {
    let home = Home::new();
    let root = home.path();
    let app = app(root);
    let exe = root.join("app.exe");
    let build = ["build", "app/hirsel.json", "-o", "app.exe"];
    // A check record does not answer the build of the same sources.
    flips_then_hits(
        &home,
        root,
        &["check", "app/hirsel.json"],
        &[],
        "check first",
    );
    let cold = home.cold(root, &build, &[]);
    let cold_bytes = std::fs::read(&exe).unwrap();
    std::fs::remove_file(&exe).unwrap();
    let (first, counts) = home.warm(root, &build, &[]);
    assert_eq!((first, counts), (cold, MISS), "build after check");
    assert_eq!(std::fs::read(&exe).unwrap(), cold_bytes, "built executable");
    use std::os::unix::fs::PermissionsExt;
    let damage: [(&str, &dyn Fn()); 4] = [
        ("unchanged", &|| {}),
        ("deleted", &|| std::fs::remove_file(&exe).unwrap()),
        ("modified", &|| {
            std::fs::write(&exe, b"not the program").unwrap()
        }),
        ("chmod -x", &|| {
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap()
        }),
    ];
    for (what, damage) in damage {
        damage();
        let (answer, counts) = home.warm(root, &build, &[]);
        assert_eq!(counts, HIT, "{what}");
        assert_eq!(answer, home.cold(root, &build, &[]), "{what}");
        assert_eq!(std::fs::read(&exe).unwrap(), cold_bytes, "{what}");
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(ran.status.code(), Some(0), "{what}: {ran:?}");
    }
    let (_, counts) = home.warm(
        root,
        &["build", "app/hirsel.json", "-o", "elsewhere.exe"],
        &[],
    );
    assert_eq!(counts, HIT, "another output path");
    assert_eq!(
        std::fs::read(root.join("elsewhere.exe")).unwrap(),
        cold_bytes
    );
    // Cross tools and memcheck tools are not inputs of an x86_64 build or of
    // a check: setting them keeps both hits.
    let tools = [
        ("HIRSEL_AARCH64_CC", "/nonexistent/cc"),
        ("HIRSEL_AARCH64_SYSROOT", "/nonexistent/sysroot"),
        ("HIRSEL_QEMU_AARCH64", "/nonexistent/qemu"),
        ("HIRSEL_VALGRIND", "/nonexistent/valgrind"),
        ("VALGRIND_LIB", "/nonexistent/lib"),
        ("HIRSEL_VALGRIND_RUNTIME", "/nonexistent/runtime"),
    ];
    let (answer, counts) = home.warm(root, &build, &tools);
    assert_eq!(
        (answer, counts),
        (home.cold(root, &build, &tools), HIT),
        "cross and memcheck tools"
    );
    // Runtime archives from the Cargo fallback are not in P: the build
    // bypasses and says why (a Cargo that fails keeps the run short).
    let fallback = [("HIRSEL_RUNTIME_ARCHIVES", UNSET), ("CARGO", "/bin/false")];
    let (_, counts) = home.warm(root, &build, &fallback);
    assert_eq!(
        counts,
        bypassed("runtime archives from the Cargo fallback"),
        "Cargo fallback"
    );
    // The build record does not answer a check.
    let (_, counts) = home.warm(root, &["check", "app/hirsel.json"], &[]);
    assert_eq!(counts, HIT, "check keeps its own record");
    let (_, counts) = home.warm(root, &["check", "app/hirsel.json"], &tools);
    assert_eq!(counts, HIT, "check with cross and memcheck tools");
    let release = ["build", "--release", "app/hirsel.json", "-o", "app.exe"];
    flips_then_hits(&home, root, &release, &[], "profile");
    // A release build captures its platform cold before the lookup; a miss
    // builds with that capture and runs no second one.
    edit(
        &app.join("main.hir"),
        "  Ok(())\n}\npub fn other",
        "  Ok(())\n}\n\npub fn other",
    );
    let (_, counts, captures) = home.report(&home.state(), root, &release, &[]);
    assert_eq!((counts, captures), (MISS, 1), "release miss");
    let (_, counts, captures) = home.report(&home.state(), root, &release, &[]);
    assert_eq!((counts, captures), (HIT, 1), "release hit");
    edit(
        &app.join("main.hir"),
        "  Ok(())\n}\n\npub fn other",
        "  Ok(())\n}\npub fn other",
    );
    // A failing build replays cold's diagnostics and status, writes no
    // executable, and is not answered by the failing check of the same sources.
    edit(&app.join("util.hir"), "x + x", "x + true");
    flips_then_hits(
        &home,
        root,
        &["check", "app/hirsel.json"],
        &[],
        "failing check",
    );
    std::fs::remove_file(&exe).unwrap();
    flips_then_hits(&home, root, &build, &[], "failing build");
    assert!(!exe.exists(), "a failing build wrote an executable");
    // A resource failure is never recorded: the next run misses and publishes.
    edit(&app.join("util.hir"), "x + true", "x + x");
    let (limited, counts) = home.warm(root, &build, &[("HIRSEL_MEMORY_LIMIT", "64M")]);
    assert_ne!(limited.status, Some(0), "{limited:?}");
    assert_eq!(
        (counts.hits, counts.publications),
        (0, 0),
        "resource failure recorded"
    );
    flips_then_hits(&home, root, &build, &[], "after resource failure");
}

/// Bug caught: a build key that leaves out the parsed proof sidecar (under
/// either `--proofs <path>` or `--proofs=<path>`), so replaced evidence is
/// answered by the record of the old evidence (program-key-proto-813 found
/// exactly this stale hit); or a hit that drops the proof report cold prints.
#[test]
fn a_build_record_keys_its_proof_evidence_and_replays_the_proof_report() {
    let home = Home::new();
    let root = home.path();
    let package = root.join("prog");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("hirsel.json"),
        r#"{"schema":1,"package":{"name":"prog","version":"1"},"entry":{"module":[],"name":"main"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[]}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("main.hir"),
        "fn bump(x: u32, y: u32) -> u32\n  ensures result > x\n{\n  x + y\n}\npub fn main() -> Result[(), ()] {\n  let n = bump(1u32, 0u32)\n  Ok(())\n}\n",
    )
    .unwrap();
    let mut loaded = hirsel_front_phases::load_program(&package.join("hirsel.json")).unwrap();
    loaded.check_for_execution().unwrap();
    let text = loaded.engine.export_tir(&loaded.entry).unwrap();
    let export = loaded.engine.import_tir(&text).unwrap();
    let hash = |h: [u8; 32]| {
        format!(
            "#{}",
            h.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    };
    let obligation = export
        .definitions
        .iter()
        .flat_map(|d| &d.obligations)
        .find(|o| o.id.to_string().ends_with("/bump#FACT-1/ensures/0"))
        .unwrap();
    let id = obligation.id.to_string();
    let sidecar = |checker: &str| {
        serde_json::json!({"format": "hirsel.proof.sidecar.v1", "records": [{
            "revision": hash(export.header.program_revision.unwrap().0),
            "obligation": &id,
            "checksum": hash(obligation.checksum.0),
            "checker": checker,
            "budget": {"unit": "steps", "count": 1},
            "verdict": "proved",
        }]})
        .to_string()
    };
    let proofs = root.join("p.json");
    std::fs::write(&proofs, sidecar("liar@1")).unwrap();
    let separated = [
        "build",
        "--proofs",
        "p.json",
        "--trust",
        "liar@1",
        "prog/hirsel.json",
        "-o",
        "prog.exe",
    ];
    let equals = [
        "build",
        "--proofs=p.json",
        "--trust",
        "liar@1",
        "prog/hirsel.json",
        "-o",
        "prog.exe",
    ];
    // The report case itself: cold prints the proof report and a hit replays
    // it byte for byte.
    let removed = format!("hirsel: proof {id}: removed\n");
    let cold = home.cold(root, &separated, &[]);
    assert!(
        String::from_utf8_lossy(&cold.stderr).starts_with(&removed),
        "{cold:?}"
    );
    flips_then_hits(&home, root, &separated, &[], "proof report");
    let (answer, counts) = home.warm(root, &equals, &[]);
    assert_eq!(
        (answer, counts),
        (home.cold(root, &equals, &[]), HIT),
        "the other spelling"
    );
    // Replaced evidence under the same path: the check is kept again, which
    // cold reports, and neither spelling may be answered by the old record.
    std::fs::write(&proofs, sidecar("other@1")).unwrap();
    flips_then_hits(&home, root, &equals, &[], "replaced sidecar (--proofs=)");
    std::fs::write(&proofs, sidecar("liar@1").replace("liar@1", "liar@2")).unwrap();
    flips_then_hits(&home, root, &separated, &[], "replaced sidecar (--proofs)");
    // Evidence that does not parse is refused before any record is read,
    // warm as cold (an unregistered home).
    std::fs::write(&proofs, "{\"format\": \"hirsel.proof.sidecar.v1\"}").unwrap();
    for args in [&separated[..], &equals[..]] {
        let (answer, counts) = home.warm(root, args, &[]);
        assert_eq!(counts.hits, 0, "invalid sidecar answered: {args:?}");
        let state = tempfile::tempdir().unwrap();
        assert_eq!(
            answer,
            home.run(state.path(), root, args, &[]).0,
            "invalid sidecar: {args:?}"
        );
        assert_ne!(answer.status, Some(0), "{answer:?}");
    }
}
