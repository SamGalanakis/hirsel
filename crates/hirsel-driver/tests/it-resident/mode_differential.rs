//! One law across request modes. Every mode at every job count must produce
//! the same canonical outputs (diagnostic JSON and TIR bytes) as a
//! from-scratch fresh request at one job, across cold, no-op and edit
//! sequences. The modes, edits and job counts are data: a new request mode
//! adds a `Mode` variant and nothing else.
use hirsel_driver::requests::{CompileOperation, CompileResult};
use hirsel_driver::session::Session;
use hirsel_engine::CompileConfig;
use hirsel_engine::memo::Retention;
use hirsel_engine::store::store_by_name;
use hirsel_intern::jobs::Jobs;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
}
const MODES: [Mode; 4] = [Mode::Fresh, Mode::Resident, Mode::Restarted, Mode::StoreOff];
const JOBS: [usize; 3] = [1, 2, 8];

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
        };
        CompileConfig {
            jobs: Jobs::new(jobs),
            retention,
            store: store_by_name(store, Some(store_root.to_path_buf())).unwrap(),
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
const STEPS: [Step; 7] = [
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
    fn request(&mut self, entry: &Path) -> Outputs {
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

#[test]
fn every_request_mode_and_job_count_matches_a_fresh_compile_across_edits() {
    hirsel_engine::with_compiler_stack(|| {
        let tree = tempfile::tempdir().unwrap();
        let entry = tree.path().join("main.hir");
        let mut cells: Vec<Cell> = MODES
            .iter()
            .flat_map(|&mode| JOBS.map(|jobs| (mode, jobs)))
            .map(|(mode, jobs)| Cell {
                mode,
                jobs,
                store_root: tree.path().join(format!("results-{mode:?}-{jobs}")),
                session: None,
            })
            .collect();
        assert_eq!((cells[0].mode, cells[0].jobs), (Mode::Fresh, 1));
        let mut divergent = Vec::new();
        for step in &STEPS {
            std::fs::write(&entry, step.source).unwrap();
            let oracle = cells[0].request(&entry);
            let artifacts = oracle
                .as_ref()
                .unwrap_or_else(|error| panic!("{}: fresh request failed: {error}", step.name));
            let diagnostics = std::str::from_utf8(&artifacts["diagnostics"]).unwrap();
            assert_eq!(
                (artifacts.contains_key("tir"), diagnostics.is_empty()),
                (step.valid, step.valid),
                "{}: fresh outputs disagree with the hand-derived validity\n{}",
                step.name,
                describe(&oracle)
            );
            for cell in &mut cells[1..] {
                let actual = cell.request(&entry);
                if actual != oracle {
                    divergent.push(format!(
                        "{} / {:?} / jobs {}:\n--- fresh jobs 1\n{}\n--- actual\n{}",
                        step.name,
                        cell.mode,
                        cell.jobs,
                        describe(&oracle),
                        describe(&actual)
                    ));
                }
            }
        }
        assert!(
            divergent.is_empty(),
            "{} divergent cells; first:\n{}",
            divergent.len(),
            divergent.join("\n\n")
        );
    });
}
