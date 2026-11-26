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

/// The grants a gate test may hold; `Dir` is allowed separately, as a relative path.
const ALLOWED: [&str; 5] = ["Clock", "Random", "Stdout", "Stderr", "Tasks"];

/// The most report bytes the gate reads from a test run; a larger report is a red run.
const REPORT_BYTES: usize = 16 << 20;

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
/// pipe, marking a cut. `None` is a cancelled run.
pub(super) fn collect(
    mut child: Child,
    cancel: &AtomicBool,
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
    let status = wait_for_compile(&mut child, cancel);
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
        .join("hirsel.toml");
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
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Failure::Invalid(format!("{}: {e}", manifest.display())))?;
    let parsed: toml::Table = text
        .parse()
        .map_err(|e| Failure::Invalid(format!("{}: {e}", manifest.display())))?;
    let Some(tests) = parsed.get("tests") else {
        return Ok(());
    };
    let denied = |test: &str, grant: &dyn std::fmt::Display| {
        Err(Failure::Denied(format!(
            "law gate: {test} may not be granted {grant}"
        )))
    };
    let check = |test: &str, config: &toml::Value, keys: &[&str]| -> Result<(), Failure> {
        let Some(config) = config.as_table() else {
            return denied(test, &"a configuration that is not a table");
        };
        for (key, value) in config {
            if !keys.contains(&key.as_str()) {
                return denied(test, &format!("{key:?}"));
            }
            let strings = || -> Result<Vec<&str>, Failure> {
                value
                    .as_array()
                    .and_then(|items| items.iter().map(toml::Value::as_str).collect())
                    .ok_or_else(|| {
                        Failure::Invalid(format!("{test}.{key} is not a list of strings"))
                    })
            };
            match key.as_str() {
                "grants" => {
                    for grant in strings()? {
                        let kind = grant.split_once('=').map_or(grant, |(kind, _)| kind);
                        if !ALLOWED.contains(&kind) {
                            return denied(test, &grant);
                        }
                    }
                }
                "dirs" => {
                    for dir in strings()? {
                        let path = Path::new(dir);
                        let inside = !dir.is_empty()
                            && hirsel_core_ids::launch::dir_slot(dir).as_deref() == Ok(dir)
                            && path
                                .components()
                                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
                        if !inside {
                            return denied(test, &format!("Dir {dir:?}"));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    };
    check(
        "tests",
        tests,
        &["sources", "limit", "grants", "dirs", "runs"],
    )?;
    if let Some(runs) = tests.get("runs") {
        let Some(runs) = runs.as_table() else {
            return denied("tests.runs", &"a configuration that is not a table");
        };
        for (name, run) in runs {
            check(
                &format!("tests.runs.{name}"),
                run,
                &["limit", "grants", "dirs"],
            )?;
        }
    }
    Ok(())
}

/// The gate's steps 1, 2 and 4 on `tree`: the authority check, then `hirsel test PACKAGE
/// --frozen --json` in a fresh scratch under the launcher's compiler and package stores and
/// the pass cache `STATE/test-results`, read as a verdict. `None` is a cancelled run.
pub(crate) fn gate(
    compiler: &Compiler,
    state: &Path,
    tree: &Tree,
    cancel: &AtomicBool,
) -> Option<Result<Verdict, Failure>> {
    if let Err(denied) = authority(tree) {
        return Some(Err(denied));
    }
    let unrun = |e: &dyn std::fmt::Display| Some(Err(Failure::Invalid(format!("law gate: {e}"))));
    let cache = state.join("test-results");
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&cache)
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        return unrun(&e);
    }
    let dir = match scratch(state, tree) {
        Ok(dir) => dir,
        Err(e) => return unrun(&e),
    };
    let path = |name: &str| state.join(name).to_string_lossy().into_owned();
    let args: Vec<String> = vec![
        "test".into(),
        tree.package.clone(),
        "--frozen".into(),
        "--json".into(),
        "--store".into(),
        path("compiler"),
        "--package-store".into(),
        path("packages"),
        "--test-cache".into(),
        path("test-results"),
    ];
    let mut command = match compiler(&args) {
        Ok(command) => command,
        Err(e) => return unrun(&e),
    };
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    command
        .current_dir(dir.path().join("tree"))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return unrun(&e),
    };
    let (_, stdout, stderr) = match collect(child, cancel, REPORT_BYTES)? {
        Ok(run) => run,
        Err(e) => return unrun(&e),
    };
    Some(Ok(verdict(&stdout, &String::from_utf8_lossy(&stderr))))
}

/// Read a `hirsel test --json` report. No report is a lost session when the resident
/// compiler ended without an answer, and otherwise a run that did not start.
fn verdict(stdout: &[u8], stderr: &str) -> Verdict {
    let Ok(report) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        if stderr.contains("the server ended without an answer") {
            return Verdict::Lost;
        }
        return Verdict::Red(vec![format!(
            "the test run did not start: {}",
            stderr.trim()
        )]);
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
