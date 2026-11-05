//! One law across request modes. Every mode at every job count must produce
//! the same canonical outputs (diagnostic JSON and TIR bytes) as a
//! from-scratch fresh request at one job, across cold, no-op and edit
//! sequences. The edits are data and each (mode, jobs) column is one case: a
//! new request mode adds a `Mode` variant and its three column cases.
use hirsel_driver::session::Session;
use hirsel_engine::CompileConfig;
use hirsel_engine::memo::Retention;
use hirsel_engine::store::store_by_name;
use hirsel_intern::jobs::Jobs;
use hirsel_requests::CompileOperation;
use hirsel_requests::CompileResult;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "../support/installed_remote.rs"]
mod installed_remote;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// A new process per request with no result store: the from-scratch oracle.
    Fresh,
    /// One kept session over a directory store.
    Resident,
    /// A new process per request reopening the same directory store.
    Restarted,
    /// One kept session with no result store.
    StoreOff,
    Remote,
}

impl Mode {
    fn kept(self) -> bool {
        matches!(self, Mode::Resident | Mode::StoreOff)
    }
    fn config(self, jobs: usize, store_root: &Path) -> CompileConfig {
        let (retention, store) = match self {
            Mode::Fresh => (Retention::Batch, "none@1"),
            Mode::Resident => (Retention::Resident, "dir@1"),
            Mode::Restarted => (Retention::Batch, "dir@1"),
            Mode::StoreOff => (Retention::Resident, "none@1"),
            Mode::Remote => (Retention::Batch, "remote@1"),
        };
        let selected = store_by_name(store, Some(store_root.to_path_buf())).unwrap();
        let store = if self == Mode::Remote {
            installed_remote::bind(selected)
        } else {
            selected
        };
        CompileConfig {
            jobs: Jobs::new(jobs),
            retention,
            store,
            ..CompileConfig::default()
        }
    }
}

/// The edit sequence, applied in order to one source file. `valid` is derived
/// by hand from the program text: whether it type-checks and exports TIR.
struct Step {
    name: &'static str,
    source: &'static str,
    valid: bool,
}
const BASE: &str = "fn helper(x:u64)->u64{x+1u64}\n\
                    pub fn twice(x:u64)->u64{helper(x)+helper(x)}\n\
                    pub fn main()->u64{twice(3u64)}\n";
const BODY: &str = "fn helper(x:u64)->u64{x+1u64}\n\
                    pub fn twice(x:u64)->u64{helper(x)*2u64}\n\
                    pub fn main()->u64{twice(3u64)}\n";
const PRIVATE: &str = "fn helper(x:u64)->u64{x+2u64}\n\
                       pub fn twice(x:u64)->u64{helper(x)*2u64}\n\
                       pub fn main()->u64{twice(3u64)}\n";
const HEADER: &str = "fn helper(x:u64)->u64{x+2u64}\n\
                      pub fn twice(x:u64,y:u64)->u64{helper(x)*y}\n\
                      pub fn main()->u64{twice(3u64,2u64)}\n";
/// `y:u32` multiplies a u64, and `main` passes a u64 to it: two type errors.
const BROKEN_HEADER: &str = "fn helper(x:u64)->u64{x+2u64}\n\
                             pub fn twice(x:u64,y:u32)->u64{helper(x)*y}\n\
                             pub fn main()->u64{twice(3u64,2u64)}\n";
/// `generated` is produced at compile time by `body()`, which reads `source()`.
const GENERATOR: &str = "fn helper(x:u64)->u64{x+2u64}\n\
                         pub fn twice(x:u64,y:u64)->u64{helper(x)*y}\n\
                         fn source()->String{\"fn(x:u64)->u64 uses {} {x+3u64}\"}\n\
                         fn body()->Result[Body[fn(u64)->u64 uses {}],String]{Ok(codegen.body(source()))}\n\
                         fn generated(x:u64)->u64 uses {} = comptime body()\n\
                         pub fn main()->u64{twice(generated(3u64),2u64)}\n";
/// Only the generator's input `source()` changes, so `generated` must too.
const GENERATOR_INPUT: &str = "fn helper(x:u64)->u64{x+2u64}\n\
                               pub fn twice(x:u64,y:u64)->u64{helper(x)*y}\n\
                               fn source()->String{\"fn(x:u64)->u64 uses {} {x+4u64}\"}\n\
                               fn body()->Result[Body[fn(u64)->u64 uses {}],String]{Ok(codegen.body(source()))}\n\
                               fn generated(x:u64)->u64 uses {} = comptime body()\n\
                               pub fn main()->u64{twice(generated(3u64),2u64)}\n";
const STEPS: [Step; 9] = [
    Step {
        name: "cold",
        source: BASE,
        valid: true,
    },
    Step {
        name: "no-op",
        source: BASE,
        valid: true,
    },
    Step {
        name: "body edit",
        source: BODY,
        valid: true,
    },
    Step {
        name: "private edit",
        source: PRIVATE,
        valid: true,
    },
    Step {
        name: "header edit",
        source: HEADER,
        valid: true,
    },
    Step {
        name: "breaking header edit",
        source: BROKEN_HEADER,
        valid: false,
    },
    Step {
        name: "repair",
        source: HEADER,
        valid: true,
    },
    Step {
        name: "generator",
        source: GENERATOR,
        valid: true,
    },
    Step {
        name: "generator input edit",
        source: GENERATOR_INPUT,
        valid: true,
    },
];

type Outputs = Result<BTreeMap<String, Vec<u8>>, String>;

fn outputs(session: &mut Session) -> Outputs {
    let output = session
        .request(CompileOperation::Artifacts { native: false })
        .map_err(|error| error.to_string())?;
    match &output.result {
        CompileResult::Artifacts(artifacts) => Ok(artifacts.clone()),
        _ => Err("artifact request returned another operation".into()),
    }
}

struct Cell {
    mode: Mode,
    jobs: usize,
    store_root: PathBuf,
    session: Option<Session>,
}
impl Cell {
    fn new(mode: Mode, jobs: usize, tree: &Path) -> Self {
        Cell {
            mode,
            jobs,
            store_root: tree.join(format!("results-{mode:?}-{jobs}")),
            session: None,
        }
    }
    fn request(&mut self, entry: &Path) -> Outputs {
        if self.mode == Mode::Remote {
            let config = self.mode.config(self.jobs, &self.store_root);
            let written = outputs(
                self.session
                    .get_or_insert_with(|| Session::open_configured(entry, config)),
            );
            let config = self.mode.config(self.jobs, &self.store_root);
            let mut reader = Session::open_configured(entry, config);
            let restored = outputs(&mut reader);
            assert_eq!(restored, written, "remote reader differs from its writer");
            if restored
                .as_ref()
                .is_ok_and(|artifacts| artifacts.contains_key("tir"))
            {
                assert!(
                    reader.store_adoptions() > 0,
                    "remote reader adopted no stored results"
                );
                let definition = reader.logical_entry().unwrap().clone();
                let inspection = reader
                    .request_applied(CompileOperation::Inspect {
                        definitions: vec![definition],
                    })
                    .unwrap();
                let CompileResult::Inspect(inspection) = &inspection.result else {
                    panic!("remote reader inspection returned another operation");
                };
                assert_eq!(
                    inspection.definitions.len(),
                    1,
                    "remote reader inspection did not return one definition"
                );
                assert!(
                    inspection.definitions[0].body_validations > 0,
                    "remote reader did not run V"
                );
            }
            return restored;
        }
        let config = self.mode.config(self.jobs, &self.store_root);
        if !self.mode.kept() {
            return outputs(&mut Session::open_configured(entry, config));
        }
        outputs(
            self.session
                .get_or_insert_with(|| Session::open_configured(entry, config)),
        )
    }
}

fn describe(outputs: &Outputs) -> String {
    match outputs {
        Err(error) => format!("error: {error}"),
        Ok(artifacts) => artifacts
            .iter()
            .map(|(name, bytes)| format!("[{name}]\n{}", String::from_utf8_lossy(bytes)))
            .collect(),
    }
}

/// One column of the law: `mode` at `jobs` against the fresh jobs-1 oracle,
/// both in one private tree so diagnostics name the same source path.
fn column(mode: Mode, jobs: usize) {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let tree = tempfile::tempdir().unwrap();
        let entry = tree.path().join("main.hir");
        let mut oracle = Cell::new(Mode::Fresh, 1, tree.path());
        let mut cell = Cell::new(mode, jobs, tree.path());
        let mut divergent = Vec::new();
        for step in &STEPS {
            std::fs::write(&entry, step.source).unwrap();
            let expected = oracle.request(&entry);
            let artifacts = expected
                .as_ref()
                .unwrap_or_else(|error| panic!("{}: fresh request failed: {error}", step.name));
            let diagnostics = std::str::from_utf8(&artifacts["diagnostics"]).unwrap();
            assert_eq!(
                (artifacts.contains_key("tir"), diagnostics.is_empty()),
                (step.valid, step.valid),
                "{}: fresh outputs disagree with the hand-derived validity\n{}",
                step.name,
                describe(&expected)
            );
            let actual = cell.request(&entry);
            if actual != expected {
                divergent.push(format!(
                    "{}:\n--- fresh jobs 1\n{}\n--- {mode:?} jobs {jobs}\n{}",
                    step.name,
                    describe(&expected),
                    describe(&actual)
                ));
            }
        }
        assert!(
            divergent.is_empty(),
            "{mode:?} jobs {jobs}: {} divergent steps\n{}",
            divergent.len(),
            divergent.join("\n\n")
        );
    });
}

// One case per column: each is its own shard (tools/buck2/sync.py CASE_SHARDED).
#[test]
fn fresh_jobs_2() {
    column(Mode::Fresh, 2)
}
#[test]
fn fresh_jobs_8() {
    column(Mode::Fresh, 8)
}
#[test]
fn resident_jobs_1() {
    column(Mode::Resident, 1)
}
#[test]
fn resident_jobs_2() {
    column(Mode::Resident, 2)
}
#[test]
fn resident_jobs_8() {
    column(Mode::Resident, 8)
}
#[test]
fn restarted_jobs_1() {
    column(Mode::Restarted, 1)
}
#[test]
fn restarted_jobs_2() {
    column(Mode::Restarted, 2)
}
#[test]
fn restarted_jobs_8() {
    column(Mode::Restarted, 8)
}
#[test]
fn store_off_jobs_1() {
    column(Mode::StoreOff, 1)
}
#[test]
fn store_off_jobs_2() {
    column(Mode::StoreOff, 2)
}
#[test]
fn store_off_jobs_8() {
    column(Mode::StoreOff, 8)
}
#[test]
fn remote_jobs_1() {
    column(Mode::Remote, 1)
}
#[test]
fn remote_jobs_2() {
    column(Mode::Remote, 2)
}
#[test]
fn remote_jobs_8() {
    column(Mode::Remote, 8)
}
