//! A built program holds exactly the bodies its launch entries reach. A body
//! is reached by a call, a function value, a closure, a constant that holds a
//! function, or a resource's finalizer; a compile-time-only generator, a test
//! and a function nothing calls are not reached. The oracle is the program text:
//! which names it reaches is read off by hand, and a run prints what the reached
//! bodies compute.
hirsel_guard::install!();
use std::path::Path;
use std::process::Command;

const MAIN: &str = r#"pub type Failure = Grant(GrantError) | Output(OutputError)

resource type Handle {
  n: u64,
} drop(h) {
  let _ = h.n
}

fn squares__generated() -> List[u64] {
  [0u64, 1u64, 4u64, 9u64]
}

const SQUARES: List[u64] = comptime squares__generated()

fn triple__through_constant(x: u64) -> u64 {
  3u64 * x
}

type Scale {
  run: fn(u64) -> u64 uses {},
}

const SCALE: Scale = Scale(run: triple__through_constant)

fn never__called(x: u64) -> u64 {
  x + 1u64
}

@test
fn checks__squares(t: Test) -> Result[(), String] {
  Ok(())
}

pub fn main(rt: Io) -> Result[(), Failure] {
  let output = io.stdout(rt) ? Grant
  let handle = Handle(n: 1u64)
  let tasks = io.tasks(rt) ? Grant
  task.scope(tasks, fn(s: Scope) -> () {
    let h = task.async(s, fn() -> u32 { 7u32 })
    let _: u32 = task.await(s, take h)
  })
  let scale = SCALE
  out.print(output, f"{scale.run(SQUARES[3])}\n") ? Output
  Ok(())
}
"#;

/// A module the edit test leaves alone; `words__tripled` starts unreached.
const WORDS: &str = r#"pub fn count(n: u64) -> u64 { n * 2u64 + 1u64 }

pub fn words__tripled(n: u64) -> u64 { n * 3u64 }
"#;

const EDIT_MAIN: &str = r#"pub type Failure = Grant(GrantError) | Output(OutputError)

pub fn main(rt: Io) -> Result[(), Failure] {
  let output = io.stdout(rt) ? Grant
  out.print(output, f"{words.count(3u64)}\n") ? Output
  Ok(())
}
"#;

fn package(directory: &Path, main: &str) {
    let package = directory.join("package");
    std::fs::create_dir_all(&package).unwrap();
    let std = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib/std@1/hirsel.json")
        .canonicalize()
        .unwrap();
    std::fs::write(
        package.join("hirsel.json"),
        serde_json::json!({
            "schema": 1,
            "package": {"name": "reachable_roots_801", "version": "1"},
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
    std::fs::write(package.join("main.hir"), main).unwrap();
    std::fs::write(package.join("words.hir"), WORDS).unwrap();
}

/// One release build into the cache `cache`; returns the program's path.
fn build(directory: &Path, cache: &str) -> std::path::PathBuf {
    let program = directory.join(format!("{cache}.program"));
    let output = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .env_remove("HOME")
        .env("XDG_CACHE_HOME", directory.join(cache))
        .env("HIRSEL_OBJECTS", "dir@1")
        .arg("build")
        .arg(directory.join("package/hirsel.json"))
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    program
}

/// Runs a built program with stdout and a task pool granted.
fn run(directory: &Path, program: &Path) -> std::process::Output {
    let grants = directory.join("grants.json");
    std::fs::write(
        &grants,
        serde_json::json!({"io": {"stdout": "stdout.bytes@1", "dirs": [], "tasks": "tasks.free@1"}})
            .to_string(),
    )
    .unwrap();
    let output = Command::new(program)
        .arg("--hirsel-grants")
        .arg(&grants)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn a_release_program_holds_exactly_the_bodies_its_entry_reaches() {
    let directory = tempfile::Builder::new()
        .prefix("hirsel-reachable-roots-801-")
        .tempdir()
        .unwrap();
    let directory = directory.path();
    package(directory, MAIN);
    let program = build(directory, "cache");
    let bytes = std::fs::read(&program).unwrap();
    // A body's link name (emission::symbol_name doubles each `_`) is in the
    // symbol table and the code-identity table exactly when the body is built.
    let holds = |name: &str| {
        let linked = name.replace('_', "__");
        bytes.windows(linked.len()).any(|w| w == linked.as_bytes())
    };
    assert!(holds("triple__through_constant"));
    for unreached in [
        "squares__generated",
        "never__called",
        "checks__squares",
        "words__tripled",
    ] {
        assert!(!holds(unreached), "{unreached} is built");
    }
    // The finalizer, the task closure and the constant's function run.
    let run = run(directory, &program);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "27\n");
}

#[test]
fn adding_the_first_call_to_an_unreached_function_equals_a_cold_build() {
    let directory = tempfile::Builder::new()
        .prefix("hirsel-reachable-roots-801-edit-")
        .tempdir()
        .unwrap();
    let directory = directory.path();
    package(directory, EDIT_MAIN);
    build(directory, "a");
    std::fs::write(
        directory.join("package/main.hir"),
        EDIT_MAIN.replace(
            "words.count(3u64)",
            "words.words__tripled(words.count(3u64))",
        ),
    )
    .unwrap();
    let edited = std::fs::read(build(directory, "a")).unwrap();
    let cold = std::fs::read(build(directory, "b")).unwrap();
    assert!(edited == cold, "the edit build differs from a cold build");
    let run = run(directory, &directory.join("b.program"));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "21\n");
}

/// A closure handed to `task.async` inside `task.scope` reaches `bump__by_value`
/// only as a function value. Its overflow must still run in the pruned build and
/// name the function's own line (FAIL-2: a child's failure continues outward).
const ESCAPING: &str = r#"pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let tasks = io.tasks(rt)?
  task.scope(
    tasks,
    fn(s: Scope) -> () {
      let h = task.async(s, fn() -> u8 { apply(bump__by_value, 255u8) })
      let value = task.await(s, take h)
    }
  )
  Ok(())
}

fn apply(f: fn(u8) -> u8 uses {}, x: u8) -> u8 uses {} {
  f(x)
}

fn bump__by_value(x: u8) -> u8 uses {} {
  x + 1u8
}
"#;

#[test]
fn a_function_reached_only_through_an_escaping_closure_runs_and_names_its_site() {
    let directory = tempfile::Builder::new()
        .prefix("hirsel-reachable-roots-801-escaping-")
        .tempdir()
        .unwrap();
    let program = directory.path().join("escaping.hir");
    std::fs::write(&program, ESCAPING).unwrap();
    let line = ESCAPING
        .lines()
        .position(|line| line.contains("x + 1u8"))
        .unwrap()
        + 1;
    let output = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .env_remove("HOME")
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .args(["run", "--native", "--json"])
        .arg(&program)
        .args([
            "--grants",
            r#"{"io":{"tasks":"tasks.free@1","dirs":[]},"args":[]}"#,
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(hirsel_run_schema::exit::FAILED),
        "{output:?}"
    );
    let failure: serde_json::Value = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("{output:?}"));
    assert_eq!(failure["runtime_failure"]["rule"], "SEM-1", "{failure}");
    assert_eq!(
        failure["runtime_failure"]["site"]["line"], line,
        "{failure}"
    );
    let definition = failure["runtime_failure"]["site"]["definition"].as_str();
    assert!(
        definition.is_some_and(|d| d.ends_with("bump__by_value")),
        "{failure}"
    );
}
