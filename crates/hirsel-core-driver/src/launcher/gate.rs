//! The law gate (docs/spec/launch.md "Self-editing", "The law gate"): a submitted tree's own
//! tests, run by `hirsel test PACKAGE --frozen` on the same files and lock it publishes from,
//! with the launcher's stores and its pass cache at `STATE/test-results`. The authority check
//! refuses, before any test runs, a test granted anything beyond the gate's allow list.
use super::{Compiler, Failure, Tree, wait_for_compile};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The most bytes of a gate run's stderr the gate keeps for its diagnostics. The report
/// itself is spooled to a file and read whole.
const DIAGNOSTIC_BYTES: usize = 256 << 10;

/// A gate run's verdict ("The verdict").
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every test passed, executed or reused; a package without tests is green.
    Green,
    /// Each test that failed, stopped or did not start, with its outcome.
    Red(Vec<String>),
    /// The resident compiler's session was lost: never green and never a durable red.
    Lost,
}

/// Write `tree.files` under a fresh scratch directory in `STATE/source`; its `tree`
/// directory is where `publish` and the gate run.
pub(super) fn scratch(state: &Path, tree: &Tree) -> io::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir_in(state.join("source"))?;
    let source = dir.path().join("tree");
    fs::create_dir(&source)?;
    for (path, bytes) in &tree.files {
        let path = source.join(path);
        fs::create_dir_all(path.parent().expect("a file under the tree"))?;
        fs::File::create_new(&path)?.write_all(bytes)?;
    }
    Ok(dir)
}

/// Wait for a child started in its own process group and keep at most `cap` bytes of each
/// pipe, marking a cut. `None` is a cancelled run; a run still going at `deadline` is
/// killed and is a `TimedOut` error.
pub(super) fn collect(
    mut child: Child,
    cancel: &AtomicBool,
    deadline: Option<Instant>,
    cap: usize,
) -> Option<io::Result<(ExitStatus, Vec<u8>, Vec<u8>)>> {
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut pipe) = pipe {
                let mut chunk = [0; 8192];
                let mut cut = false;
                while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                    cut |= n > room;
                }
                if cut {
                    kept.extend_from_slice(
                        format!("\n[output cut at {} KiB]\n", cap >> 10).as_bytes(),
                    );
                }
            }
            kept
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = wait_for_compile(&mut child, cancel, deadline);
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    Some(status.map(|status| (status, stdout, stderr)))
}

/// The authority check ("before any test runs"): every test's run configuration in the
/// package's `[tests]` grants only the allow list and relative `Dir`s without `..`, and
/// names nothing else (no corpus, no recording). A violation is `Denied`, naming the test
/// (`tests` for the package default, `tests.runs.NAME` for one test) and the grant.
pub(super) fn authority(tree: &Tree) -> Result<(), Failure> {
    let package = Path::new(&tree.package);
    let manifest = package
        .strip_prefix(".")
        .unwrap_or(package)
        .join(crate::package_dir::MANIFEST);
    let Some((_, bytes)) = tree
        .files
        .iter()
        .find(|(name, _)| Path::new(name) == manifest)
    else {
        return Err(Failure::Invalid(format!(
            "the tree has no {}",
            manifest.display()
        )));
    };
    use hirsel_config_schema::pkg_v1::{Directory, Grant, GrantChange};
    let parsed =
        crate::package_manifest::Manifest::parse(bytes, &manifest).map_err(Failure::Invalid)?;
    let Some(tests) = parsed.package.and_then(|p| p.tests) else {
        return Ok(());
    };
    fn check(test: &str, grant: &Grant) -> Result<(), Failure> {
        let kind = match grant {
            Grant::Clock | Grant::Random | Grant::Stdout | Grant::Stderr | Grant::Tasks => {
                return Ok(());
            }
            Grant::Dir(dir) => {
                if !dir.is_empty()
                    && !dir.contains('\0')
                    && Path::new(dir)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
                {
                    return Ok(());
                }
                format!("Dir {dir:?}")
            }
            Grant::ScopedDir { .. } => "ScopedDir".into(),
            Grant::Stdin => "Stdin".into(),
            Grant::Process => "Process".into(),
            Grant::Net(_) => "Net".into(),
            Grant::Exec { .. } => "Exec".into(),
            Grant::Foreign(_) => "Foreign".into(),
            Grant::Env(_) => "Env".into(),
            Grant::Secret { .. } => "Secret".into(),
            Grant::Cred { .. } => "Cred".into(),
            Grant::Compiler => "Compiler".into(),
        };
        Err(Failure::Denied(format!(
            "law gate: {test} may not be granted {kind}"
        )))
    }
    for grant in &tests.grants {
        check("tests", grant)?;
    }
    for (name, run) in tests.runs {
        let test = format!("tests.runs.{name}");
        match run.grant_change {
            None => {}
            Some(GrantChange::ReplaceAll(values)) => {
                for grant in &values {
                    check(&test, grant)?;
                }
            }
            Some(GrantChange::ReplaceDirs(values)) => {
                for dir in values {
                    let grant = match dir {
                        Directory::Dir(p) => Grant::Dir(p),
                        Directory::ScopedDir { kind, app } => Grant::ScopedDir { kind, app },
                    };
                    check(&test, &grant)?;
                }
            }
            Some(GrantChange::ReplaceNonDir(values)) => {
                for grant in values {
                    check(&test, &crate::package_dir::grants::non_dir(grant))?;
                }
            }
        }
    }
    Ok(())
}

/// The gate's steps 2 and 4 on `tree`, after its authority check: `hirsel check PACKAGE
/// --frozen` over the tests' sources, then `hirsel test PACKAGE --frozen --json`, both in
/// `source`, the publish scratch's tree, under the launcher's compiler and package stores
/// and the pass cache `STATE/test-results`, read as a verdict. The test run's report is
/// spooled to `report.json` beside `source`, never cut. A run still going at `deadline`
/// is red. `None` is a cancelled run.
pub(crate) fn gate(
    compiler: &Compiler,
    state: &Path,
    tree: &Tree,
    source: &Path,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Option<Result<Verdict, Failure>> {
    let unrun = |e: &dyn std::fmt::Display| Some(Err(Failure::Invalid(format!("law gate: {e}"))));
    let cache = state.join("test-results");
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&cache)
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        return unrun(&e);
    }
    let path = |name: &str| state.join(name).to_string_lossy().into_owned();
    let stores = [
        tree.package.clone(),
        crate::package_lock::FROZEN_FLAG.into(),
        "--store".into(),
        path("compiler"),
        "--package-store".into(),
        path("packages"),
    ];
    let run = |verb: &str, more: &[String], report: Option<fs::File>| {
        let args: Vec<String> = [verb.to_owned()]
            .into_iter()
            .chain(stores.iter().cloned())
            .chain(more.iter().cloned())
            .collect();
        let mut command = match compiler(&args) {
            Ok(command) => command,
            Err(e) => return Some(Err(e)),
        };
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        command
            .current_dir(source)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(report.map_or_else(Stdio::piped, Stdio::from))
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => collect(child, cancel, Some(deadline), DIAGNOSTIC_BYTES),
            Err(e) => Some(Err(e)),
        }
    };
    // `hirsel test` lists the tests of the modules that parse, and publish checks only the
    // program: a test source that does not check would drop its tests from the run unseen.
    let late = || {
        Some(Ok(Verdict::Red(vec![
            "the tests had not all ended by the admission deadline".into(),
        ])))
    };
    let (status, _, stderr) = match run("check", &[], None)? {
        Ok(run) => run,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => return late(),
        Err(e) => return unrun(&e),
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Some(Ok(Verdict::Red(vec![format!(
            "the package's tests do not check: {}",
            stderr.trim()
        )])));
    }
    let Some(spool) = source.parent().map(|dir| dir.join("report.json")) else {
        return unrun(&"the source tree has no scratch directory");
    };
    let report = match fs::File::create_new(&spool) {
        Ok(file) => file,
        Err(e) => return unrun(&e),
    };
    let more = ["--json".into(), "--test-cache".into(), path("test-results")];
    let (_, _, stderr) = match run("test", &more, Some(report))? {
        Ok(run) => run,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => return late(),
        Err(e) => return unrun(&e),
    };
    let report = match fs::File::open(&spool).and_then(|file| {
        let bytes = file.metadata()?.len();
        Ok((bytes, serde_json::from_reader(io::BufReader::new(file))))
    }) {
        Ok(report) => report,
        Err(e) => return unrun(&e),
    };
    Some(Ok(verdict(report, &String::from_utf8_lossy(&stderr))))
}

/// Read a `hirsel test --json` report of `bytes` bytes, parsed. An unparseable report is a
/// lost session when the resident compiler ended without an answer; otherwise no report
/// is a run that did not start, and a report that is not JSON is red, saying where.
fn verdict((bytes, report): (u64, serde_json::Result<serde_json::Value>), stderr: &str) -> Verdict {
    let report = match report {
        Ok(report) => report,
        Err(_) if stderr.contains("the server ended without an answer") => return Verdict::Lost,
        Err(_) if bytes == 0 => {
            return Verdict::Red(vec![format!(
                "the test run did not start: {}",
                stderr.trim()
            )]);
        }
        Err(e) => {
            return Verdict::Red(vec![format!(
                "the test run's report of {bytes} bytes is not JSON ({e}): {}",
                stderr.trim()
            )]);
        }
    };
    let Some(rows) = report["tests"].as_array() else {
        return Verdict::Red(vec![format!(
            "the test run reported no tests: {}",
            stderr.trim()
        )]);
    };
    let red: Vec<String> = rows
        .iter()
        .filter(|row| row["passed"] != true)
        .map(|row| {
            let test = row["definition"].as_str().unwrap_or("a test");
            let outcome = if row["timed_out"] == true {
                "stopped at its deadline".to_owned()
            } else if let Some(stopped) = row["stopped"].as_str() {
                format!("did not start: {stopped}")
            } else {
                match &row["status"] {
                    serde_json::Value::Number(status) => format!("failed with status {status}"),
                    _ => "stopped".to_owned(),
                }
            };
            format!("{test}: {outcome}")
        })
        .collect();
    if red.is_empty() && (report["passed"] == true || rows.is_empty()) {
        Verdict::Green
    } else if red.is_empty() {
        Verdict::Red(vec![format!("the test run failed: {}", stderr.trim())])
    } else {
        Verdict::Red(red)
    }
}
