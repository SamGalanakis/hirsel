//! A one-body edit rebuilds exactly the object that holds the body: an
//! ordinary object keys only its own bodies' continuation plans, and the edit
//! replans one body. The independent oracle for the rebuilt program is a cold
//! build of the edited tree into an empty cache.
hirsel_guard::install!();
use std::path::Path;
use std::process::Command;

const MAIN: &str = r#"pub type Failure = Grant(GrantError) | Output(OutputError)

pub fn main(rt: Io) -> Result[(), Failure] {
  let output = io.stdout(rt) ? Grant
  out.print(output, "hello, world\n") ? Output
  out.print(output, f"{words.count(3u64)}\n") ? Output
  Ok(())
}
"#;
/// Another module the edit leaves alone; it also reaches a print boundary.
const WORDS: &str = r#"pub fn count(n: u64) -> u64 { n * 2u64 + 1u64 }
"#;

/// One `--debug` build into `cache`; returns the timing counts and the program.
fn build(directory: &Path, cache: &str) -> (serde_json::Value, Vec<u8>) {
    let program = directory.join(format!("{cache}.program"));
    let output = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .env_remove("HOME")
        .env("XDG_CACHE_HOME", directory.join(cache))
        .env("HIRSEL_OBJECTS", "dir@1")
        .args(["build", "--debug", "--timings=json"])
        .arg(directory.join("package/hirsel.json"))
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    let report: serde_json::Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    (report["counts"].clone(), std::fs::read(&program).unwrap())
}

fn count(counts: &serde_json::Value, name: &str) -> u64 {
    counts[name].as_u64().unwrap_or(0)
}

#[test]
fn a_one_body_edit_rebuilds_one_object_and_equals_a_cold_build() {
    let directory = tempfile::Builder::new()
        .prefix("hirsel-object-edit-737-")
        .tempdir()
        .unwrap();
    let directory = directory.path();
    let package = directory.join("package");
    std::fs::create_dir(&package).unwrap();
    let std = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib/std@1/hirsel.json")
        .canonicalize()
        .unwrap();
    std::fs::write(
        package.join("hirsel.json"),
        serde_json::json!({
            "schema": 1,
            "package": {"name": "object_edit_737", "version": "1"},
            "sources": [
                {"path": "main.hir", "module": []},
                {"path": "words.hir", "module": ["words"]}
            ],
            "dependencies": [{
                "manifest": std,
                "package": {"name": "std", "version": "1"},
                "expose": []
            }],
            "entry": {"module": [], "name": "main"}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(package.join("main.hir"), MAIN).unwrap();
    std::fs::write(package.join("words.hir"), WORDS).unwrap();

    let (cold, _) = build(directory, "a");
    let objects = count(&cold, "object misses");
    assert!(objects > 1, "{cold}");
    assert_eq!(count(&cold, "object hits"), 0, "{cold}");

    // One edited literal in `main`: only main's object is rebuilt.
    std::fs::write(
        package.join("main.hir"),
        MAIN.replace("hello, world", "hello, there"),
    )
    .unwrap();
    let (edit, program) = build(directory, "a");
    assert_eq!(count(&edit, "object misses"), 1, "{edit}");
    assert_eq!(count(&edit, "object hits"), objects - 1, "{edit}");

    let (fresh, oracle) = build(directory, "b");
    assert_eq!(count(&fresh, "object hits"), 0, "{fresh}");
    assert!(
        program == oracle,
        "the edit build differs from a cold build"
    );
}
