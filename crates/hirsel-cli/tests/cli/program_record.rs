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
    compiler: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
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
        // Absolute: Buck2 names the binary relative to the test's own directory.
        Self::with_compiler(std::path::absolute(env!("CARGO_BIN_EXE_hirsel")).unwrap())
    }
    fn with_compiler(compiler: PathBuf) -> Self {
        let home = Self {
            root: tempfile::tempdir().unwrap(),
            compiler,
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
        let mut command = Command::new(&self.compiler);
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
        self.report_in(state, cwd, args, env, b"")
    }
    /// [`Home::report`] with `input` on the command's stdin.
    fn report_in(
        &self,
        state: &Path,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
    ) -> (Answer, Counts, u64) {
        let mut child = self
            .request(state, cwd, args, env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let writer = std::thread::spawn(move || {
            use std::io::Write;
            // A program that stops reading early closes the pipe.
            let _ = stdin.write_all(&input);
        });
        let output: Output = child.wait_with_output().unwrap();
        writer.join().unwrap();
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
    /// The command of one request, with its timings report.
    fn request(&self, state: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = self.command(state);
        command.current_dir(cwd).arg("--timings=json").args(args);
        for (name, value) in env {
            match *value {
                UNSET => command.env_remove(name),
                value => command.env(name, value),
            };
        }
        command
    }
    /// One run whose stdout and stderr write one file, as a terminal shows
    /// both: the bytes in the order written, without the timings report
    /// (the last line), and the record counts.
    fn merged(&self, state: &Path, cwd: &Path, args: &[&str]) -> (Vec<u8>, Counts) {
        let file = tempfile::tempfile().unwrap();
        let status = self
            .request(state, cwd, args, &[])
            .stdout(file.try_clone().unwrap())
            .stderr(file.try_clone().unwrap())
            .status()
            .unwrap();
        let mut bytes = vec![];
        use std::io::{Read, Seek};
        let mut file = file;
        file.rewind().unwrap();
        file.read_to_end(&mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let (rest, report) = text
            .trim_end_matches('\n')
            .rsplit_once('\n')
            .unwrap_or(("", &text));
        let report: serde_json::Value = serde_json::from_str(report).unwrap();
        let count = |name: &str| report["counts"][name].as_u64().unwrap();
        (
            format!("{status}\n{rest}").into_bytes(),
            Counts {
                hits: count("program record hits"),
                misses: count("program record misses"),
                publications: count("program record publications"),
                bypass: report["program_record_bypass"].as_str().map(str::to_owned),
            },
        )
    }
    /// `merged` under the forced cold oracle.
    fn merged_cold(&self, cwd: &Path, args: &[&str]) -> Vec<u8> {
        let state = tempfile::tempdir().unwrap();
        let (bytes, counts) = self.merged(state.path(), cwd, args);
        assert_eq!(counts, bypassed("no registration"), "the cold oracle");
        bytes
    }
    fn warm(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (Answer, Counts) {
        self.run(&self.state(), cwd, args, env)
    }
    fn warm_in(&self, cwd: &Path, args: &[&str], input: &[u8]) -> (Answer, Counts) {
        let (answer, counts, _) = self.report_in(&self.state(), cwd, args, &[], input);
        (answer, counts)
    }
    /// The forced cold oracle: a state home with no registration.
    fn cold(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Answer {
        self.cold_in(cwd, args, env, b"")
    }
    fn cold_in(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> Answer {
        let state = tempfile::tempdir().unwrap();
        let (answer, counts, _) = self.report_in(state.path(), cwd, args, env, input);
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
    // Two errors in two packages replay in cold's order at either width.
    for jobs in ["1", "2"] {
        let env = [("HIRSEL_JOBS", jobs)];
        let (answer, counts) = home.warm(root, &check, &env);
        assert_eq!(
            (answer, counts),
            (home.cold(root, &check, &env), HIT),
            "two errors, jobs {jobs}"
        );
    }
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
        // `--json` writes diagnostics to stdout and its summary to stderr:
        // a replay must interleave the two streams as cold does.
        let cold = home.merged_cold(root, display);
        assert_eq!(
            home.merged(&home.state(), root, display),
            (cold, HIT),
            "{display:?}, both streams in one"
        );
    }
    let plain = home.cold(root, &check, &[]);
    assert!(
        plain.stderr.split(|b| *b == b'\n').count() > 4,
        "multi-line diagnostics"
    );
    edit(&root.join("dep/dep.hir"), "  1u64 + true", "  1u64");
    edit(&app.join("util.hir"), "x + true", "x + x");
    flips_then_hits(&home, root, &check, &[], "body byte (accepted)");
    // A check that succeeds with a warning (LANG-8: a private error type's
    // arm nothing constructs) replays the warning in every renderer.
    let warning = "\ntype UtilError = Used | Unused\nfn guarded(x: u64) -> Result[u64, UtilError] {\n  if x == 0u64 {\n    return Error(Used())\n  }\n  Ok(x)\n}\n";
    edit(
        &app.join("util.hir"),
        "  x + x\n}\n",
        &format!("  x + x\n}}\n{warning}"),
    );
    flips_then_hits(&home, root, &check, &[], "warning");
    for display in [&check[..], &json[..]] {
        let cold = home.merged_cold(root, display);
        let text = String::from_utf8_lossy(&cold);
        assert!(
            text.starts_with("exit status: 0") && text.contains("LANG-8"),
            "{display:?}: {text}"
        );
        assert_eq!(
            home.merged(&home.state(), root, display),
            (cold, HIT),
            "{display:?}, warning"
        );
    }
    edit(&app.join("util.hir"), warning, "");
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
        // Followed: a Buck2 output tree links its files.
        if std::fs::metadata(entry.path()).unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Bug caught: a build hit that writes other bytes than the build recorded,
/// leaves a damaged, deleted or non-executable output in place, keeps a
/// record whose executable is damaged (every later build misses), answers from
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
        // The hit's own bytes, before the cold oracle rewrites the output.
        assert_eq!(std::fs::read(&exe).unwrap(), cold_bytes, "{what}");
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(ran.status.code(), Some(0), "{what}: {ran:?}");
        assert_eq!(answer, home.cold(root, &build, &[]), "{what}");
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
    // A damaged recorded executable: the build runs fresh and its
    // publication replaces the record, so the next build hits again.
    for path in recorded_executables(&home) {
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
    let cold = home.cold(root, &build, &[]);
    // An output that already holds the recorded bytes is kept unread.
    std::fs::remove_file(&exe).unwrap();
    let (answer, counts) = home.warm(root, &build, &[]);
    assert_eq!((answer, counts), (cold.clone(), MISS), "damaged record");
    assert_eq!(std::fs::read(&exe).unwrap(), cold_bytes, "damaged record");
    let (answer, counts) = home.warm(root, &build, &[]);
    assert_eq!((answer, counts), (cold, HIT), "damaged record replaced");
    // Stage validation revalidates every build: under either spelling a
    // build neither reads nor publishes a record.
    let validated = [
        "build",
        "--validate-stages",
        "app/hirsel.json",
        "-o",
        "app.exe",
    ];
    let (_, counts) = home.warm(root, &validated, &[]);
    assert_eq!(counts, bypassed("validate stages"), "--validate-stages");
    let (_, counts) = home.warm(root, &validated, &[]);
    assert_eq!(
        counts,
        bypassed("validate stages"),
        "--validate-stages again"
    );
    // The CLI turns the environment spelling into the same validation.
    let (_, counts) = home.warm(root, &build, &[("HIRSEL_VALIDATE_STAGES", "1")]);
    assert_eq!(
        counts,
        bypassed("validate stages"),
        "HIRSEL_VALIDATE_STAGES=1"
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

/// The built compiler copied beside its own copy of libLLVM, which it finds
/// through its DT_RPATH (`$ORIGIN/../../../../../../tools/buck2/toolchains/
/// llvm-files/lib`, every other library a link to the checkout's), with the
/// standard library it finds upward from itself: a test can change the
/// libLLVM bytes this compiler maps.
struct Relocated {
    _root: tempfile::TempDir,
    compiler: PathBuf,
    llvm: PathBuf,
}
impl Relocated {
    fn new() -> Self {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let built = std::fs::canonicalize(env!("CARGO_BIN_EXE_hirsel")).unwrap();
        let checkout = built.parent().unwrap().join("../../../../../..");
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("b/1/2/3/4/5");
        std::fs::create_dir_all(&bin).unwrap();
        let compiler = bin.join("hirsel");
        std::fs::copy(&built, &compiler).unwrap();
        let source = checkout.join("tools/buck2/toolchains/llvm-files/lib");
        let lib = root.path().join("tools/buck2/toolchains/llvm-files/lib");
        std::fs::create_dir_all(&lib).unwrap();
        for entry in std::fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            symlink(entry.path(), lib.join(entry.file_name())).unwrap();
        }
        let llvm = lib.join("libLLVM.so.23.1");
        std::fs::remove_file(&llvm).unwrap();
        std::fs::copy(source.join("libLLVM.so.23.1"), &llvm).unwrap();
        std::fs::set_permissions(&llvm, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(checkout.join("stdlib"), root.path().join("stdlib")).unwrap();
        Self {
            _root: root,
            compiler,
            llvm,
        }
    }
    /// Other libLLVM bytes at the same path and inode: one byte appended
    /// past everything the loader maps.
    fn change_llvm(&self) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&self.llvm)
            .unwrap()
            .write_all(b"\0")
            .unwrap();
    }
}

/// Bug caught: a check keyed by what only code generation reads (the libLLVM
/// the compiler maps, a native tool variable), so it misses after either
/// changes; or a build or run success key without them, so a build or an
/// LLVM run by other LLVM code, or with another link kit, is answered by the
/// old record.
#[test]
fn only_a_build_or_run_keys_the_mapped_libllvm_and_native_tools() {
    let relocated = Relocated::new();
    let home = Home::with_compiler(relocated.compiler.clone());
    let root = home.path();
    app(root);
    let check = ["check", "app/hirsel.json"];
    let build = ["build", "app/hirsel.json", "-o", "app.exe"];
    flips_then_hits(&home, root, &check, &[], "check");
    flips_then_hits(&home, root, &build, &[], "build");
    pipe(root);
    let run = [
        "run",
        "--native",
        "--backend",
        "llvm",
        "pipe/hirsel.json",
        "--grants",
        "pipe/grants.json",
        "--",
        "a",
    ];
    flips_then_hits(&home, root, &run, &[], "run");
    relocated.change_llvm();
    let (answer, counts) = home.warm(root, &check, &[]);
    assert_eq!(
        (answer, counts),
        (home.cold(root, &check, &[]), HIT),
        "check, other libLLVM bytes"
    );
    flips_then_hits(&home, root, &build, &[], "build, other libLLVM bytes");
    flips_then_hits(&home, root, &run, &[], "run, other libLLVM bytes");
    let kit = std::env::var_os("HIRSEL_LINK_KIT").expect("the build tests' link kit");
    let link = root.join("kit");
    std::os::unix::fs::symlink(&kit, &link).unwrap();
    let env = [("HIRSEL_LINK_KIT", link.to_str().unwrap())];
    let (answer, counts) = home.warm(root, &check, &env);
    assert_eq!(
        (answer, counts),
        (home.cold(root, &check, &env), HIT),
        "check, another link kit path"
    );
    flips_then_hits(&home, root, &build, &env, "build, another link kit path");
    flips_then_hits(&home, root, &run, &env, "run, another link kit path");
}

/// A program that prints its arguments, then up to 64 bytes of its stdin, then
/// a line on stderr, and traps when given three or more arguments.
fn pipe(root: &Path) -> PathBuf {
    let pipe = root.join("pipe");
    std::fs::create_dir_all(&pipe).unwrap();
    std::fs::write(
        pipe.join("main.hir"),
        r#"pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let input = io.stdin(rt)?
  let output = io.stdout(rt)?
  let diagnostics = io.stderr(rt)?
  for i in 0u64..arg.len(args) {
    let text = match arg.text(args, i) { Ok(t) => t, Error(_) => "?" }
    let _ = out.print(output, "arg ")
    let _ = out.print(output, text)
    let _ = out.print(output, "\n")
  }
  let mut buffer: List[u8] = []
  let _ = stdin.read(input, mut buffer, 64u64)
  let _ = out.write(output, bytes.from_list(take buffer))
  let _ = err.print(diagnostics, "done\n")
  failure.check(arg.len(args) < 3, Arithmetic())
  Ok(())
}

pub fn other(rt: Io, args: Args) -> Result[(), GrantError] {
  let output = io.stdout(rt)?
  let _ = out.print(output, "other\n")
  main(rt, args)
}
"#,
    )
    .unwrap();
    std::fs::write(pipe.join("hirsel.json"), r#"{"schema":1,"package":{"name":"pipe","version":"1"},"entry":{"module":[],"name":"main"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[]}"#).unwrap();
    std::fs::write(
        pipe.join("grants.json"),
        r#"{"io":{"stdin":"stdin.bytes@1","stdout":"stdout.bytes@1","stderr":"stderr.bytes@1","dirs":[]},"args":[]}"#,
    )
    .unwrap();
    pipe
}

/// The one recorded executable of a run record, found by its ELF magic in
/// the record domain.
fn recorded_executables(home: &Home) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else if std::fs::read(&path).is_ok_and(|bytes| bytes.starts_with(b"\x7fELF")) {
                found.push(path);
            }
        }
    }
    let mut found = vec![];
    walk(&home.state().join("hirsel/verifier"), &mut found);
    found
}

/// Bug caught: a run hit that launches with the recorded run's stdin or
/// arguments instead of this run's, a run key without the launch inputs the
/// linker embeds (other arguments answered by the old executable), an exit
/// status taken from the record, a run answered by a build or check record
/// or the reverse, a hit that launches a damaged recorded executable, or a
/// launch-only variable that changes the key.
#[test]
fn a_run_record_launches_its_executable_with_this_runs_inputs() {
    let home = Home::new();
    let root = home.path();
    pipe(root);
    let run = |args: &[&'static str]| -> Vec<&'static str> {
        let mut all = vec![
            "run",
            "--native",
            "pipe/hirsel.json",
            "--grants",
            "pipe/grants.json",
            "--",
        ];
        all.extend_from_slice(args);
        all
    };
    let one = run(&["a"]);
    // Check and build records of the same sources do not answer the run.
    flips_then_hits(&home, root, &["check", "pipe/hirsel.json"], &[], "check");
    flips_then_hits(
        &home,
        root,
        &["build", "pipe/hirsel.json", "-o", "pipe.exe"],
        &[],
        "build",
    );
    let cold = home.cold_in(root, &one, &[], b"first\n");
    assert_eq!(cold.status, Some(0), "{cold:?}");
    assert_eq!(cold.stdout, b"arg a\nfirst\n", "{cold:?}");
    let (answer, counts) = home.warm_in(root, &one, b"first\n");
    assert_eq!((answer, counts), (cold, MISS), "first run");
    // Other stdin: the record answers, and the program reads this run's input.
    for input in [&b"second\n"[..], b"", b"\x00\xff\n"] {
        let (answer, counts) = home.warm_in(root, &one, input);
        assert_eq!(
            (answer, counts),
            (home.cold_in(root, &one, &[], input), HIT),
            "stdin {input:?}"
        );
    }
    // Other arguments are other launch inputs: the linker embeds the bound
    // run, so the first run with them misses and publishes, then hits.
    let two = run(&["a", "b"]);
    let cold = home.cold_in(root, &two, &[], b"x");
    assert_eq!(cold.stdout, b"arg a\narg b\nx", "{cold:?}");
    let (answer, counts) = home.warm_in(root, &two, b"x");
    assert_eq!((answer, counts), (cold.clone(), MISS), "other arguments");
    let (answer, counts) = home.warm_in(root, &two, b"x");
    assert_eq!((answer, counts), (cold, HIT), "other arguments again");
    // A run that traps is recorded like any built run: its exit status is
    // the program's, never the record's.
    let three = run(&["a", "b", "c"]);
    let cold = home.cold_in(root, &three, &[], b"y");
    assert_ne!(cold.status, Some(0), "{cold:?}");
    let (answer, counts) = home.warm_in(root, &three, b"y");
    assert_eq!((answer, counts), (cold.clone(), MISS), "trap");
    let (answer, counts) = home.warm_in(root, &three, b"y");
    assert_eq!((answer, counts), (cold, HIT), "trap again");
    // An LLVM run lives at the address its backend names: on its own
    // backend it misses, publishes and hits, and the build of the same
    // sources with the same backend keeps its own record.
    let llvm = [&["run", "--native", "--backend", "llvm"][..], &one[2..]].concat();
    let cold = home.cold_in(root, &llvm, &[], b"l");
    assert_eq!(cold.stdout, b"arg a\nl", "{cold:?}");
    let (answer, counts) = home.warm_in(root, &llvm, b"l");
    assert_eq!((answer, counts), (cold.clone(), MISS), "llvm run");
    let (answer, counts) = home.warm_in(root, &llvm, b"l");
    assert_eq!((answer, counts), (cold, HIT), "llvm run again");
    // The check and build records still answer their own operations.
    let (_, counts) = home.warm(root, &["check", "pipe/hirsel.json"], &[]);
    assert_eq!(counts, HIT, "check after runs");
    let (_, counts) = home.warm(root, &["build", "pipe/hirsel.json", "-o", "pipe.exe"], &[]);
    assert_eq!(counts, HIT, "build after runs");
    // One record per address: the last run's arguments replaced the first
    // run's record, so the first arguments miss once and are recorded again.
    let (answer, counts) = home.warm_in(root, &one, b"z");
    assert_eq!(
        (answer, counts),
        (home.cold_in(root, &one, &[], b"z"), MISS),
        "first arguments again"
    );
    // Launch-only variables: run --native is x86_64 only, so no emulator is
    // selected, and memcheck runtimes are test --native's. Neither keys a run
    // nor changes what it launches.
    let tools = [
        ("HIRSEL_QEMU_AARCH64", "/nonexistent/qemu"),
        ("HIRSEL_VALGRIND", "/nonexistent/valgrind"),
        ("VALGRIND_LIB", "/nonexistent/lib"),
        ("HIRSEL_VALGRIND_RUNTIME", "/nonexistent/runtime"),
    ];
    let (answer, counts, _) = home.report_in(&home.state(), root, &one, &tools, b"z");
    assert_eq!(
        (answer, counts),
        (home.cold_in(root, &one, &tools, b"z"), HIT),
        "launch-only variables"
    );
    // Kept native scratch is for a build's work: such a run never answers
    // from the record, so no kept directory belongs to a hit.
    let scratch = tempfile::tempdir().unwrap();
    let keep = [
        ("HIRSEL_KEEP_NATIVE_SCRATCH", "1"),
        ("TMPDIR", scratch.path().to_str().unwrap()),
    ];
    let (answer, counts, _) = home.report_in(&home.state(), root, &one, &keep, b"z");
    assert_eq!(
        counts,
        bypassed("variable HIRSEL_KEEP_NATIVE_SCRATCH is a bypass mode"),
        "kept scratch"
    );
    assert_eq!(
        (answer.status, answer.stdout),
        (Some(0), b"arg a\nz".to_vec()),
        "kept scratch"
    );
    // A damaged recorded executable never launches: the run is answered
    // fresh, as cold, and its publication replaces the damaged record, so the
    // next run hits again.
    use std::os::unix::fs::PermissionsExt;
    let recorded = recorded_executables(&home);
    assert!(!recorded.is_empty(), "the run record holds its executable");
    let damage: [(&str, &dyn Fn(&Path)); 3] = [
        ("chmod -x", &|path| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap()
        }),
        ("modified", &|path| {
            let mut bytes = std::fs::read(path).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(path, permissions).unwrap();
            std::fs::write(path, bytes).unwrap();
        }),
        ("deleted", &|path| std::fs::remove_file(path).unwrap()),
    ];
    for (what, damage) in damage {
        for path in &recorded {
            if path.exists() {
                damage(path);
            }
        }
        let cold = home.cold_in(root, &one, &[], b"w");
        let (answer, counts) = home.warm_in(root, &one, b"w");
        if what == "chmod -x" {
            // The record's bytes are copied out and made executable.
            assert_eq!((answer, counts), (cold, HIT), "{what}");
        } else {
            assert_eq!((answer, counts), (cold.clone(), MISS), "{what}");
            let (answer, counts) = home.warm_in(root, &one, b"w");
            assert_eq!((answer, counts), (cold, HIT), "{what}: replaced");
        }
    }
    // Flips with this run's inputs, each answer compared with a cold run.
    let flips = |env: &[(&str, &str)], input: &[u8], what: &str| {
        let cold = home.cold_in(root, &one, env, input);
        let (answer, counts, _) = home.report_in(&home.state(), root, &one, env, input);
        assert_eq!((answer, counts), (cold.clone(), MISS), "{what}");
        let (answer, counts, _) = home.report_in(&home.state(), root, &one, env, input);
        assert_eq!((answer, counts), (cold, HIT), "{what}: rerun");
    };
    // A regenerated std image (the installed products registered again).
    home.register("pub fn regenerated() -> u8 {\n  9u8\n}\n");
    flips(&[], b"r", "std image regenerated");
    // The runtime archives: first at another path, then at that same path
    // with other bytes (the first member header's date, which no link reads).
    let archives =
        std::path::absolute(std::env::var_os("HIRSEL_RUNTIME_ARCHIVES").expect("runtime archives"))
            .unwrap();
    let runtime = root.join("runtime");
    copy_tree(&archives, &runtime);
    let env = [("HIRSEL_RUNTIME_ARCHIVES", runtime.to_str().unwrap())];
    flips(&env, b"p", "runtime archives at another path");
    fn grow(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                grow(&path);
            } else if path.extension().is_some_and(|e| e == "a") {
                let mut bytes = std::fs::read(&path).unwrap();
                assert!(bytes.starts_with(b"!<arch>\n"), "{path:?}");
                // `!<arch>\n`, then the member name (16 bytes), then its date (12).
                bytes[24..36].copy_from_slice(b"1234567890  ");
                let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                permissions.set_readonly(false);
                std::fs::set_permissions(&path, permissions).unwrap();
                std::fs::write(&path, bytes).unwrap();
            }
        }
    }
    grow(&runtime);
    flips(&env, b"q", "runtime archives replaced at the same path");
    // What the program is: a body edit, a manifest that names another
    // entry, then other grants-file bytes. Each misses and launches the new
    // program, as a cold run of the edited files does.
    let edit = |path: &str, from: &str, to: &str| {
        let path = root.join(path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{path:?}");
        std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    };
    edit("pipe/main.hir", r#""done\n""#, r#""fin\n""#);
    let cold = home.cold_in(root, &one, &env, b"s");
    let stderr = String::from_utf8_lossy(&cold.stderr);
    assert!(
        stderr.contains("fin\n") && !stderr.contains("done"),
        "{cold:?}"
    );
    flips(&env, b"s", "source body edit");
    edit("pipe/hirsel.json", r#""name":"main""#, r#""name":"other""#);
    let cold = home.cold_in(root, &one, &env, b"m");
    assert_eq!(cold.stdout, b"other\narg a\nm", "{cold:?}");
    flips(&env, b"m", "manifest entry edit");
    edit("pipe/grants.json", r#""args":[]"#, r#""args": []"#);
    flips(&env, b"g", "grants file bytes");
    // A tape and a revocation file are read at launch, not keyed: a hit
    // records a tape that replays to the cold run's answer.
    std::fs::write(
        root.join("pipe/revoked.json"),
        r#"{"schema":1,"revoked":[]}"#,
    )
    .unwrap();
    let taped = |tape: &'static str| {
        [
            &one[..5],
            &["--record", tape, "--revocations", "pipe/revoked.json"][..],
            &one[5..],
        ]
        .concat()
    };
    let cold = home.cold_in(root, &taped("cold.tape"), &env, b"t");
    let (answer, counts, _) = home.report_in(&home.state(), root, &taped("hit.tape"), &env, b"t");
    assert_eq!(
        (answer, counts),
        (cold.clone(), HIT),
        "tape and revocations"
    );
    let replay = [
        "run",
        "--native",
        "pipe/hirsel.json",
        "--replay",
        "hit.tape",
    ];
    let (answer, counts, _) = home.report_in(&home.state(), root, &replay, &env, b"");
    assert_eq!(
        (answer.status, answer.stdout, counts),
        (
            cold.status,
            cold.stdout,
            bypassed("replay or callee launch")
        ),
        "the hit's tape replayed"
    );
    // A granted directory's host path enters the bound run canonicalized, so
    // retargeting a symlink changes the run the executable embeds while the
    // grants bytes, and so the key, stay equal: the hit's bound identity
    // differs from R's, the run is answered fresh and replaces the record.
    for dir in ["d1", "d2"] {
        std::fs::create_dir_all(root.join("pipe").join(dir)).unwrap();
    }
    std::os::unix::fs::symlink("d1", root.join("pipe/d")).unwrap();
    std::fs::write(
        root.join("pipe/dirs.json"),
        r#"{"io":{"stdin":"stdin.bytes@1","stdout":"stdout.bytes@1","stderr":"stderr.bytes@1","dirs":[{"path":"d","host":"d","access":"read"}]},"args":[]}"#,
    )
    .unwrap();
    let dirs = [&one[..4], &["pipe/dirs.json"][..], &one[5..]].concat();
    let cold = home.cold_in(root, &dirs, &env, b"d");
    let (answer, counts, _) = home.report_in(&home.state(), root, &dirs, &env, b"d");
    assert_eq!((answer, counts), (cold.clone(), MISS), "granted directory");
    let (answer, counts, _) = home.report_in(&home.state(), root, &dirs, &env, b"d");
    assert_eq!((answer, counts), (cold, HIT), "granted directory again");
    std::fs::remove_file(root.join("pipe/d")).unwrap();
    std::os::unix::fs::symlink("d2", root.join("pipe/d")).unwrap();
    let cold = home.cold_in(root, &dirs, &env, b"d");
    let (answer, counts, _) = home.report_in(&home.state(), root, &dirs, &env, b"d");
    assert_eq!(
        (answer, counts),
        (cold.clone(), MISS),
        "granted directory retargeted"
    );
    let (answer, counts, _) = home.report_in(&home.state(), root, &dirs, &env, b"d");
    assert_eq!(
        (answer, counts),
        (cold, HIT),
        "granted directory retargeted, replaced"
    );
}
