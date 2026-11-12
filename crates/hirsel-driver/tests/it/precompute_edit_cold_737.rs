//! A Precompute's published installation is a function of its inputs: a run
//! into a root that already holds an earlier tree's products publishes exactly
//! what a cold run of the same tree publishes — the installation (its rows and
//! the store entries and contents they list) and the rows V registers.
//! The independent oracle is a cold run into an empty root.
use hirsel_driver::CompilerRequestsExt;
use hirsel_front_phases::COMPILER;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CHILD: &str = "HIRSEL_PRECOMPUTE_EDIT_COLD_737";
const SOURCE: &str = r#"fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body("fn()->u8 uses {} {7u8}"))}
pub fn main()->u8 uses {} = comptime make()
pub fn second()->u64{9u64}
pub fn third()->u64{second()}
"#;
/// A second file the edit leaves alone: its records are hits in the edit run.
const UNCHANGED: &str = "pub fn fourth()->u64{third()}\n";
const MANIFEST: &str = r#"{"schema":1,"package":{"name":"edit_cold_737","version":"1"},"sources":[{"path":"main.hir","module":[]},{"path":"unchanged.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#;

fn installation(root: &Path) -> hirsel_artifact_store::Association {
    hirsel_artifact_store::Store::new(root.to_owned())
        .lookup_owned(hirsel_artifact_store::Address(
            hirsel_syntax::ContentHash::of_bytes(b"hirsel.installed-package.v5"),
        ))
        .unwrap()
}

/// Mirrors V's registration payload (after its 32-byte tag).
#[derive(serde::Deserialize)]
struct Registration {
    _state: PathBuf,
    root: PathBuf,
    _filesystem: (u64, u64),
    _verifier: hirsel_syntax::ContentHash,
    _header: (
        hirsel_syntax::ContentHash,
        hirsel_syntax::ContentHash,
        hirsel_artifact_store::Reference,
    ),
    _generation: String,
    rows: BTreeMap<hirsel_syntax::ContentHash, hirsel_artifact_store::Reference>,
}

/// The rows V registered for `root`.
fn registered(
    state: &Path,
    root: &Path,
) -> BTreeMap<hirsel_syntax::ContentHash, hirsel_artifact_store::Reference> {
    let root = root.canonicalize().unwrap();
    let mut found = None;
    let mut pending = vec![state.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "registration")
            {
                let bytes = std::fs::read(&path).unwrap();
                let registration: Registration = bincode::deserialize(&bytes[32..]).unwrap();
                if registration.root == root {
                    assert!(found.replace(registration.rows).is_none());
                }
            }
        }
    }
    found.unwrap()
}

/// One Precompute in a fresh process; returns how many full V runs it made.
fn precompute(directory: &Path, root: &Path) -> u64 {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "precompute_edit_cold_737::an_edit_run_publishes_what_a_cold_run_of_the_same_tree_publishes",
            "--nocapture",
        ])
        .env(CHILD, root)
        .env("HIRSEL_EDIT_COLD_737_MANIFEST", directory.join("pkg/hirsel.json"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("HIRSEL_RESULTS", "none@1")
        .env_remove("HIRSEL_VERIFY_CACHE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}: {output:?}", root.display());
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        // The harness may print the test name on the same line.
        .find_map(|line| line.split_once("V runs: ").map(|(_, runs)| runs))
        .unwrap_or_else(|| panic!("{}: {stdout}", root.display()))
        .parse()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("pkg")).unwrap();
    std::fs::write(directory.path().join("pkg/main.hir"), SOURCE).unwrap();
    std::fs::write(directory.path().join("pkg/unchanged.hir"), UNCHANGED).unwrap();
    std::fs::write(directory.path().join("pkg/hirsel.json"), MANIFEST).unwrap();
    directory
}

#[test]
fn an_edit_run_publishes_what_a_cold_run_of_the_same_tree_publishes() {
    if let Some(root) = std::env::var_os(CHILD) {
        use hirsel_intern::timings::{self, Count};
        let manifest = PathBuf::from(std::env::var_os("HIRSEL_EDIT_COLD_737_MANIFEST").unwrap());
        timings::enable();
        hirsel_intern::jobs::with_compiler_stack(|| {
            COMPILER
                .precompute_package(&manifest, Path::new(&root))
                .unwrap()
        });
        let runs = timings::report()
            .counts
            .into_iter()
            .find(|(count, _)| *count == Count::VRuns)
            .unwrap()
            .1;
        println!("V runs: {runs}");
        return;
    }
    let directory = fixture();
    let directory = directory.path();
    let source = directory.join("pkg/main.hir");
    let state = directory.join("state");
    let (a, b) = (directory.join("a"), directory.join("b"));

    precompute(directory, &a);
    let original = (installation(&a), registered(&state, &a));

    // One edited body: the edit run into A against a cold run into B.
    std::fs::write(&source, SOURCE.replace("9u64", "8u64")).unwrap();
    precompute(directory, &a);
    precompute(directory, &b);
    let edited = installation(&a);
    assert_ne!(edited, original.0, "the edit changes the installation");
    assert_eq!(edited, installation(&b), "edit run equals the cold run");
    assert_eq!(registered(&state, &a), registered(&state, &b), "V rows");

    // Editing back publishes the original cold installation again, although
    // A now also holds the edited tree's variants.
    std::fs::write(&source, SOURCE).unwrap();
    precompute(directory, &a);
    assert_eq!(
        installation(&a),
        original.0,
        "edit back equals the original cold run"
    );
    assert_eq!(
        registered(&state, &a),
        original.1,
        "V rows after editing back"
    );
}

/// V's verdict on a definition whose identity, policy and reads are unchanged
/// is reused across precompute generations: after editing one body whose
/// signature stays the same, only that definition is validated again. The
/// hand-derived oracle: `second`'s body is the only changed artifact; `third`
/// and `fourth` read `second`'s and `third`'s views, which the edit leaves
/// alone, and `main`'s generated body does not mention `second`.
#[test]
fn an_edit_run_validates_only_the_edited_definition() {
    let directory = fixture();
    let directory = directory.path();
    let a = directory.join("a");
    let cold = precompute(directory, &a);
    assert!(cold > 1, "the cold run validates every definition: {cold}");
    let source = directory.join("pkg/main.hir");
    std::fs::write(&source, SOURCE.replace("9u64", "8u64")).unwrap();
    assert_eq!(
        precompute(directory, &a),
        1,
        "edit run V runs (cold: {cold})"
    );
}
