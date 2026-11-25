//! The published coverage formats (`docs/coverage.md`, "Files"): the profile
//! document, which `--coverage-out`, `--json` and `hirsel coverage` share, and
//! the site map (`.covmap`) a coverage test build writes beside each
//! executable. Both hold placed rows, so nothing here reads source: the
//! offline tool merges, reports and diffs with no compiler state.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod cli;
pub(crate) mod report;

pub(crate) use report::{Report, Targets};

/// The profile format this hirsel writes and reads ("The profile").
pub(crate) const PROFILE_VERSION: u64 = 3;
/// The site map format this hirsel writes and reads ("The site map").
pub(crate) const MAP_VERSION: u64 = 2;

/// An input `hirsel coverage` or a merge refuses: unreadable, of another
/// format version, or in conflict with another input. It names the conflict.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

pub(crate) type Result<T> = std::result::Result<T, Refusal>;

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(Refusal(message.into()))
}

/// The build a run ran on.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Build {
    pub engine: String,
    pub optimize: bool,
    pub target: String,
}

impl Build {
    pub(crate) fn of(options: &crate::NativeOptions) -> Self {
        Self {
            engine: match options.backend {
                crate::Backend::Cranelift => "cranelift",
                crate::Backend::Llvm => "llvm",
            }
            .to_owned(),
            optimize: options.optimize,
            target: options.target.clone(),
        }
    }

    fn json(&self) -> Value {
        json!({"engine": self.engine, "optimize": self.optimize, "target": self.target})
    }

    fn read(row: &Value) -> Result<Self> {
        Ok(Self {
            engine: text(row, "engine")?,
            optimize: field(row, "optimize")?
                .as_bool()
                .ok_or_else(|| Refusal("`optimize` is not a boolean".into()))?,
            target: text(row, "target")?,
        })
    }
}

/// Where a row sits in source: `file` relative to its package root, 1-based
/// lines and columns. Each part is absent when the row has none.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Place {
    pub file: Option<String>,
    pub line: Option<u64>,
    pub col: Option<u64>,
    pub end_line: Option<u64>,
    pub end_col: Option<u64>,
    pub arm_line: Option<u64>,
    pub arm_col: Option<u64>,
}

const PLACE: [&str; 6] = ["line", "col", "end_line", "end_col", "arm_line", "arm_col"];

impl Place {
    fn numbers(&self) -> [Option<u64>; 6] {
        [
            self.line,
            self.col,
            self.end_line,
            self.end_col,
            self.arm_line,
            self.arm_col,
        ]
    }

    fn write(&self, row: &mut Value) {
        if let Some(file) = &self.file {
            row["file"] = file.clone().into();
        }
        for (name, value) in PLACE.into_iter().zip(self.numbers()) {
            if let Some(value) = value {
                row[name] = value.into();
            }
        }
    }

    fn read(row: &Value) -> Result<Self> {
        let number = |name| optional(row, name, Value::as_u64);
        Ok(Self {
            file: optional(row, "file", |v| v.as_str().map(str::to_owned))?,
            line: number("line")?,
            col: number("col")?,
            end_line: number("end_line")?,
            end_col: number("end_col")?,
            arm_line: number("arm_line")?,
            arm_col: number("arm_col")?,
        })
    }
}

/// The shape of an arm ("Shapes"): what its source does, so the report can
/// tell fault paths from logic no input has driven yet. An ordinary arm has
/// none.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Shape {
    /// The error arm of a `?` or `?=>`.
    Error,
    /// An arm whose first source code is inside a `fail(...)` call or a
    /// `return Error(...)`: it leaves the function with a failure at once.
    Fail,
    /// An arm of a `match` guard's test.
    Guard,
}

impl Shape {
    pub(crate) const ALL: [Self; 3] = [Self::Error, Self::Fail, Self::Guard];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Fail => "fail",
            Self::Guard => "guard",
        }
    }

    fn read(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.name() == name)
    }
}

/// One site ("Sites"): its arm (none for an entry), the definition that
/// names it, the function whose body holds it, its place, its shape, and the
/// observed runs that reached it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Site {
    pub arm: Option<String>,
    pub definition: String,
    pub function: String,
    pub place: Place,
    pub shape: Option<Shape>,
    pub passed_runs: u64,
    pub failed_runs: u64,
}

impl Site {
    pub(crate) fn entry(&self) -> bool {
        self.arm.is_none()
    }

    pub(crate) fn reached(&self) -> u64 {
        self.passed_runs + self.failed_runs
    }

    /// The row without its counts: what a site map holds for it.
    fn row(&self, key: &str) -> Value {
        let mut row = json!({"site": key, "kind": if self.entry() { "entry" } else { "arm" }});
        if let Some(arm) = &self.arm {
            row["arm"] = arm.clone().into();
        }
        row["definition"] = self.definition.clone().into();
        if !self.entry() {
            row["function"] = self.function.clone().into();
        }
        self.place.write(&mut row);
        if let Some(shape) = self.shape {
            row["shape"] = shape.name().into();
        }
        row
    }

    fn read(row: &Value) -> Result<(String, Self)> {
        let key = text(row, "site")?;
        let definition = text(row, "definition")?;
        let (arm, function) = match text(row, "kind")?.as_str() {
            "entry" => (None, definition.clone()),
            "arm" => (Some(text(row, "arm")?), text(row, "function")?),
            kind => return refuse(format!("site {key} has kind {kind}")),
        };
        let site = Self {
            arm,
            definition,
            function,
            place: Place::read(row)?,
            shape: match optional(row, "shape", |v| v.as_str().map(str::to_owned))? {
                None => None,
                Some(name) => Some(
                    Shape::read(&name)
                        .ok_or_else(|| Refusal(format!("site {key} has shape {name}")))?,
                ),
            },
            passed_runs: optional(row, "passed_runs", Value::as_u64)?.unwrap_or(0),
            failed_runs: optional(row, "failed_runs", Value::as_u64)?.unwrap_or(0),
        };
        Ok((key, site))
    }

    /// The same site at the same place, whatever its counts and whichever
    /// body holds it: one decision inlined into two functions is one site.
    fn same_place(&self, other: &Self) -> bool {
        (&self.arm, &self.definition, &self.place, self.shape)
            == (&other.arm, &other.definition, &other.place, other.shape)
    }
}

/// A function that holds sites: the hash of its source (as `hirsel.laws`
/// hashes it) and its place.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Function {
    pub src: Option<String>,
    pub place: Place,
}

impl Function {
    fn row(&self, definition: &str) -> Value {
        let mut row = json!({"definition": definition, "src": self.src});
        self.place.write(&mut row);
        row
    }

    fn read(row: &Value) -> Result<(String, Self)> {
        Ok((
            text(row, "definition")?,
            Self {
                src: optional(row, "src", |v| v.as_str().map(str::to_owned))?,
                place: Place::read(row)?,
            },
        ))
    }
}

/// One test on one build: the seeds of its runs, observed or not (`None`
/// for a run without `--sim`), its observed runs, and the sites they reached.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TestRow {
    pub seeds: BTreeSet<Option<u64>>,
    pub runs: u64,
    pub failed_runs: u64,
    pub reached: BTreeSet<String>,
}

/// One observed run, kept under `--coverage-runs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunRow {
    pub passed: bool,
    pub reached: BTreeSet<String>,
}

/// A run: its test, seed and build.
pub(crate) type RunKey = (String, Option<u64>, Build);

/// What one run reached, for its test row.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunCounts {
    pub functions: usize,
    pub arms: usize,
}

/// A profile ("The profile"): site counts over observed runs, each test's
/// seeds and reached sites on each build, and, under `--coverage-runs`, each
/// observed run. Every field is in the document, so [`Profile::from_json`]
/// gives back the profile [`Profile::json`] wrote.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Profile {
    pub scope: BTreeSet<String>,
    pub functions: BTreeMap<String, Function>,
    pub sites: BTreeMap<String, Site>,
    pub unreferenced: BTreeMap<String, Place>,
    pub tests: BTreeMap<(String, Build), TestRow>,
    pub run_rows: Option<BTreeMap<RunKey, RunRow>>,
    pub runs: u64,
    pub failed_runs: u64,
    pub unobserved_runs: u64,
}

impl Profile {
    /// The profile of one selected run. `observed` is each site the run's
    /// map holds, with whether the run set its flag, or none for an
    /// unobserved run. A key held twice (two ids that share a site) is
    /// reached when either flag is set.
    pub(crate) fn of_run(
        build: &Build,
        test: &str,
        seed: Option<u64>,
        passed: bool,
        observed: Option<Vec<(String, Site, bool)>>,
        keep_run: bool,
    ) -> (Self, Option<RunCounts>) {
        let mut profile = Self::default();
        let row = profile
            .tests
            .entry((test.to_owned(), build.clone()))
            .or_default();
        row.seeds.insert(seed);
        let Some(observed) = observed else {
            profile.unobserved_runs = 1;
            profile.run_rows = keep_run.then(BTreeMap::new);
            return (profile, None);
        };
        for (key, site, flag) in observed {
            profile.sites.entry(key.clone()).or_insert(site);
            if flag {
                row.reached.insert(key);
            }
        }
        let mut counts = RunCounts {
            functions: 0,
            arms: 0,
        };
        for key in &row.reached {
            let site = profile.sites.get_mut(key).unwrap();
            *if passed {
                &mut site.passed_runs
            } else {
                &mut site.failed_runs
            } = 1;
            *if site.entry() {
                &mut counts.functions
            } else {
                &mut counts.arms
            } += 1;
        }
        row.runs = 1;
        row.failed_runs = u64::from(!passed);
        let reached = row.reached.clone();
        profile.run_rows = keep_run.then(|| {
            BTreeMap::from([(
                (test.to_owned(), seed, build.clone()),
                RunRow { passed, reached },
            )])
        });
        profile.runs = 1;
        profile.failed_runs = u64::from(!passed);
        (profile, Some(counts))
    }

    /// The builds of the profile's runs, in order.
    pub(crate) fn builds(&self) -> BTreeSet<&Build> {
        self.tests.keys().map(|(_, build)| build).collect()
    }

    /// Add `other` ("Merge"): sites match by key, counts add, reached sets
    /// and seeds join. Refused, naming the conflict, when a run is in both
    /// (a count cannot be taken back), when one function has two sources,
    /// or when one site is placed differently: the inputs come from builds
    /// of different sources. Run rows are kept only when both sides kept
    /// them.
    pub(crate) fn merge(&mut self, other: Self) -> Result<()> {
        for (definition, function) in &other.functions {
            if let Some(held) = self.functions.get(definition)
                && held.src != function.src
            {
                return refuse(format!(
                    "{definition} has source {} in one input and {} in another: they are builds of different sources",
                    held.src.as_deref().unwrap_or("none"),
                    function.src.as_deref().unwrap_or("none")
                ));
            }
        }
        for (key, site) in &other.sites {
            if let Some(held) = self.sites.get(key)
                && !held.same_place(site)
            {
                return refuse(format!(
                    "site {key} is placed differently in two inputs: they are builds of different sources"
                ));
            }
        }
        for ((test, build), row) in &other.tests {
            if let Some(held) = self.tests.get(&(test.clone(), build.clone()))
                && let Some(seed) = held.seeds.intersection(&row.seeds).next()
            {
                let seed = seed.map_or_else(String::new, |seed| format!(" at seed {seed}"));
                return refuse(format!(
                    "the run of {test}{seed} on {} is in both profiles",
                    build.engine
                ));
            }
        }
        let empty = self.tests.is_empty();
        self.run_rows = match (self.run_rows.take(), other.run_rows) {
            (Some(mut held), Some(rows)) => {
                held.extend(rows);
                Some(held)
            }
            (None, rows) if empty => rows,
            _ => None,
        };
        self.scope.extend(other.scope);
        for (definition, function) in other.functions {
            self.functions.entry(definition).or_insert(function);
        }
        for (key, site) in other.sites {
            match self.sites.entry(key) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(site);
                }
                std::collections::btree_map::Entry::Occupied(mut held) => {
                    held.get_mut().passed_runs += site.passed_runs;
                    held.get_mut().failed_runs += site.failed_runs;
                }
            }
        }
        for (definition, place) in other.unreferenced {
            self.unreferenced.entry(definition).or_insert(place);
        }
        let functions = &self.functions;
        self.unreferenced.retain(|f, _| !functions.contains_key(f));
        for (test, row) in other.tests {
            let held = self.tests.entry(test).or_default();
            held.seeds.extend(row.seeds);
            held.runs += row.runs;
            held.failed_runs += row.failed_runs;
            held.reached.extend(row.reached);
        }
        self.runs += other.runs;
        self.failed_runs += other.failed_runs;
        self.unobserved_runs += other.unobserved_runs;
        Ok(())
    }

    /// Each test's reached sites over every build.
    pub(crate) fn reached_by_test(&self) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut tests = BTreeMap::<&str, BTreeSet<&str>>::new();
        for ((test, _), row) in &self.tests {
            tests
                .entry(test)
                .or_default()
                .extend(row.reached.iter().map(String::as_str));
        }
        tests
    }

    /// The number of sites each test reached and no other test did.
    pub(crate) fn only(&self) -> BTreeMap<&str, usize> {
        let tests = self.reached_by_test();
        let mut holders = BTreeMap::<&str, usize>::new();
        for key in tests.values().flatten() {
            *holders.entry(key).or_default() += 1;
        }
        tests
            .iter()
            .map(|(test, reached)| (*test, reached.iter().filter(|k| holders[*k] == 1).count()))
            .collect()
    }

    /// Every selected run's (test, seed) pair.
    fn pairs(&self) -> BTreeSet<(&str, Option<u64>)> {
        self.tests
            .iter()
            .flat_map(|((test, _), row)| row.seeds.iter().map(move |seed| (test.as_str(), *seed)))
            .collect()
    }

    /// The first 8 bytes, in hex, of SHA-256 over the sorted (definition,
    /// seed) pair of every selected run, each part framed by its length; a
    /// seed is its eight little-endian bytes, and no seed is empty.
    pub(crate) fn digest(&self) -> String {
        use sha2::Digest as _;
        let mut hash = sha2::Sha256::new();
        for (test, seed) in self.pairs() {
            crate::law_lock::part(&mut hash, test.as_bytes());
            crate::law_lock::part(
                &mut hash,
                &seed.map_or_else(Vec::new, |s| s.to_le_bytes().to_vec()),
            );
        }
        crate::law_lock::short(hash.finalize().as_ref())
    }

    fn selection(&self) -> Value {
        let tests: BTreeSet<_> = self.tests.keys().map(|(test, _)| test).collect();
        json!({"tests": tests.len(), "runs": self.pairs().len(), "digest": self.digest()})
    }

    /// The profile's own fields of the document. A report adds the fields it
    /// derives from them ([`report`]); [`Profile::from_json`] reads only these.
    pub(crate) fn json(&self) -> Value {
        let builds: Vec<&Build> = self.builds().into_iter().collect();
        let index = |build: &Build| builds.iter().position(|b| *b == build).unwrap();
        let keys: Vec<&String> = self.sites.keys().collect();
        let mut document = json!({
            "hirsel_coverage": PROFILE_VERSION,
            "builds": builds.iter().map(|b| b.json()).collect::<Vec<_>>(),
            "scope": self.scope,
            "selection": self.selection(),
            "runs": self.runs, "failed_runs": self.failed_runs, "unobserved_runs": self.unobserved_runs,
            "functions": self.functions.iter().map(|(f, function)| function.row(f)).collect::<Vec<_>>(),
            "sites": self.sites.iter().map(|(key, site)| {
                let mut row = site.row(key);
                row["passed_runs"] = site.passed_runs.into();
                row["failed_runs"] = site.failed_runs.into();
                row
            }).collect::<Vec<_>>(),
            "unreferenced": self.unreferenced.iter().map(|(f, place)| {
                let mut row = json!({"definition": f});
                place.write(&mut row);
                row
            }).collect::<Vec<_>>(),
            "tests": self.tests.iter().map(|((test, build), row)| json!({
                "definition": test,
                "build": index(build),
                "seeds": seeds(&row.seeds),
                "runs": row.runs, "failed_runs": row.failed_runs,
                "reached": crate::call::base64(&bitset(&keys, &row.reached)),
            })).collect::<Vec<_>>(),
        });
        if let Some(rows) = &self.run_rows {
            document["run_rows"] = rows
                .iter()
                .map(|((test, seed, build), row)| {
                    json!({
                        "definition": test, "seed": seed, "build": index(build),
                        "passed": row.passed,
                        "reached": crate::call::base64(&bitset(&keys, &row.reached)),
                    })
                })
                .collect();
        }
        document
    }

    /// The profile a document holds: the inverse of [`Profile::json`], for
    /// `--coverage-out` files, the `--json` report's `coverage` object and
    /// `hirsel coverage` input alike. Fields a report derives are ignored.
    /// A document of another format version is refused.
    pub(crate) fn from_json(document: &Value) -> Result<Self> {
        match document.get("hirsel_coverage").and_then(Value::as_u64) {
            Some(PROFILE_VERSION) => {}
            Some(version) => {
                return refuse(format!(
                    "profile format {version}; this hirsel reads format {PROFILE_VERSION}"
                ));
            }
            None => return refuse("not a coverage profile"),
        }
        let builds = rows(document, "builds")?
            .iter()
            .map(Build::read)
            .collect::<Result<Vec<_>>>()?;
        let build = |row: &Value| -> Result<Build> {
            let index = field(row, "build")?.as_u64().unwrap_or(u64::MAX);
            builds
                .get(usize::try_from(index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| Refusal(format!("build {index} is not in `builds`")))
        };
        let mut profile = Self {
            scope: rows(document, "scope")?
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| Refusal("`scope` holds a non-string".into()))?,
            functions: rows(document, "functions")?
                .iter()
                .map(Function::read)
                .collect::<Result<_>>()?,
            sites: rows(document, "sites")?
                .iter()
                .map(Site::read)
                .collect::<Result<_>>()?,
            unreferenced: rows(document, "unreferenced")?
                .iter()
                .map(|row| Ok((text(row, "definition")?, Place::read(row)?)))
                .collect::<Result<_>>()?,
            runs: number(document, "runs")?,
            failed_runs: number(document, "failed_runs")?,
            unobserved_runs: number(document, "unobserved_runs")?,
            ..Self::default()
        };
        let keys: Vec<&String> = profile.sites.keys().collect();
        let mut tests = BTreeMap::new();
        for row in rows(document, "tests")? {
            let test = TestRow {
                seeds: read_seeds(field(row, "seeds")?)?,
                runs: number(row, "runs")?,
                failed_runs: number(row, "failed_runs")?,
                reached: unbitset(&keys, &text(row, "reached")?)?,
            };
            if tests
                .insert((text(row, "definition")?, build(row)?), test)
                .is_some()
            {
                return refuse("a test and build has two rows");
            }
        }
        profile.run_rows = match document.get("run_rows") {
            None => None,
            Some(_) => Some(
                rows(document, "run_rows")?
                    .iter()
                    .map(|row| {
                        let seed = optional(row, "seed", Value::as_u64)?;
                        let passed = field(row, "passed")?
                            .as_bool()
                            .ok_or_else(|| Refusal("`passed` is not a boolean".into()))?;
                        let reached = unbitset(&keys, &text(row, "reached")?)?;
                        Ok((
                            (text(row, "definition")?, seed, build(row)?),
                            RunRow { passed, reached },
                        ))
                    })
                    .collect::<Result<_>>()?,
            ),
        };
        profile.tests = tests;
        Ok(profile)
    }
}

/// A coverage executable's site map ("The site map"): each record id with
/// its sites in `coverage::sites` order, placed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SiteMap {
    pub test: String,
    pub build: Build,
    pub scope: BTreeSet<String>,
    pub functions: BTreeMap<String, Function>,
    pub records: Vec<(String, Vec<(String, Site)>)>,
    pub unreferenced: BTreeMap<String, Place>,
}

impl SiteMap {
    pub(crate) fn json(&self) -> Value {
        json!({
            "hirsel_covmap": MAP_VERSION,
            "test": self.test,
            "build": self.build.json(),
            "scope": self.scope,
            "functions": self.functions.iter().map(|(f, function)| function.row(f)).collect::<Vec<_>>(),
            "records": self.records.iter().map(|(id, sites)| json!({
                "id": id,
                "sites": sites.iter().map(|(key, site)| site.row(key)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "unreferenced": self.unreferenced.iter().map(|(f, place)| {
                let mut row = json!({"definition": f});
                place.write(&mut row);
                row
            }).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn from_json(document: &Value) -> Result<Self> {
        match document.get("hirsel_covmap").and_then(Value::as_u64) {
            Some(MAP_VERSION) => {}
            Some(version) => {
                return refuse(format!(
                    "site map format {version}; this hirsel reads format {MAP_VERSION}"
                ));
            }
            None => return refuse("not a coverage site map"),
        }
        Ok(Self {
            test: text(document, "test")?,
            build: Build::read(field(document, "build")?)?,
            scope: rows(document, "scope")?
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| Refusal("`scope` holds a non-string".into()))?,
            functions: rows(document, "functions")?
                .iter()
                .map(Function::read)
                .collect::<Result<_>>()?,
            records: rows(document, "records")?
                .iter()
                .map(|record| {
                    let sites = rows(record, "sites")?
                        .iter()
                        .map(Site::read)
                        .collect::<Result<_>>()?;
                    Ok((text(record, "id")?, sites))
                })
                .collect::<Result<_>>()?,
            unreferenced: rows(document, "unreferenced")?
                .iter()
                .map(|row| Ok((text(row, "definition")?, Place::read(row)?)))
                .collect::<Result<_>>()?,
        })
    }

    /// A run of this map's executable: the profile its raw file gives, with
    /// `flags` the OR-ed flags of each id the file holds. A raw id the map
    /// does not hold, or one with another count of flags, is refused.
    pub(crate) fn run(
        &self,
        mut flags: BTreeMap<String, Vec<u8>>,
        seed: Option<u64>,
        passed: bool,
        keep_run: bool,
    ) -> Result<(Profile, Option<RunCounts>)> {
        let mut observed = Vec::new();
        for (id, sites) in &self.records {
            let set = flags.remove(id).unwrap_or_else(|| vec![0; sites.len()]);
            if set.len() != sites.len() {
                return refuse(format!(
                    "record {id} has {} flags and {} sites",
                    set.len(),
                    sites.len()
                ));
            }
            for ((key, site), flag) in sites.iter().zip(set) {
                observed.push((key.clone(), site.clone(), flag != 0));
            }
        }
        if let Some(id) = flags.keys().next() {
            return refuse(format!("record {id} is in no site map"));
        }
        let (mut profile, counts) = Profile::of_run(
            &self.build,
            &self.test,
            seed,
            passed,
            Some(observed),
            keep_run,
        );
        profile.scope = self.scope.clone();
        let reached: BTreeSet<&str> = profile
            .sites
            .values()
            .map(|s| s.function.as_str())
            .collect();
        profile.functions = self
            .functions
            .iter()
            .filter(|(f, _)| reached.contains(f.as_str()))
            .map(|(f, function)| (f.clone(), function.clone()))
            .collect();
        profile.unreferenced = self.unreferenced.clone();
        Ok((profile, counts))
    }
}

fn field<'a>(row: &'a Value, name: &str) -> Result<&'a Value> {
    row.get(name)
        .ok_or_else(|| Refusal(format!("a row has no `{name}`")))
}

fn text(row: &Value, name: &str) -> Result<String> {
    field(row, name)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Refusal(format!("`{name}` is not a string")))
}

fn number(row: &Value, name: &str) -> Result<u64> {
    field(row, name)?
        .as_u64()
        .ok_or_else(|| Refusal(format!("`{name}` is not a count")))
}

fn rows<'a>(row: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    field(row, name)?
        .as_array()
        .ok_or_else(|| Refusal(format!("`{name}` is not a list")))
}

/// An optional field: absent and null read as none.
fn optional<T>(row: &Value, name: &str, read: impl Fn(&Value) -> Option<T>) -> Result<Option<T>> {
    match row.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => read(value)
            .map(Some)
            .ok_or_else(|| Refusal(format!("`{name}` has the wrong type"))),
    }
}

/// Seeds as sorted, merged, inclusive ranges, after `null` for a run
/// without one.
fn seeds(seeds: &BTreeSet<Option<u64>>) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut range: Option<[u64; 2]> = None;
    for seed in seeds {
        match (seed, &mut range) {
            (None, _) => out.push(Value::Null),
            (Some(seed), Some(held)) if held[1].checked_add(1) == Some(*seed) => held[1] = *seed,
            (Some(seed), held) => {
                if let Some(done) = held.replace([*seed, *seed]) {
                    out.push(json!(done));
                }
            }
        }
    }
    out.extend(range.map(|range| json!(range)));
    Value::Array(out)
}

fn read_seeds(value: &Value) -> Result<BTreeSet<Option<u64>>> {
    let bad = || Refusal("`seeds` is not a list of null and [first, last] ranges".into());
    let mut seeds = BTreeSet::new();
    for item in value.as_array().ok_or_else(bad)? {
        match item {
            Value::Null => {
                seeds.insert(None);
            }
            Value::Array(range) => {
                let [Some(first), Some(last)] =
                    [range.first(), range.get(1)].map(|v| v.and_then(Value::as_u64))
                else {
                    return Err(bad());
                };
                if range.len() != 2 || first > last {
                    return Err(bad());
                }
                seeds.extend((first..=last).map(Some));
            }
            _ => return Err(bad()),
        }
    }
    Ok(seeds)
}

/// Bit `i` set when `keys[i]` is reached, least significant bit first.
pub(crate) fn bitset(keys: &[&String], reached: &BTreeSet<String>) -> Vec<u8> {
    let mut bytes = vec![0u8; keys.len().div_ceil(8)];
    for (i, key) in keys.iter().enumerate() {
        if reached.contains(*key) {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    bytes
}

/// The keys a base64 bitset over `keys` reaches. Refused unless it has
/// exactly `ceil(keys / 8)` bytes with unused high bits zero.
fn unbitset(keys: &[&String], text: &str) -> Result<BTreeSet<String>> {
    let bytes = unbase64(text).ok_or_else(|| Refusal(format!("`{text}` is not base64")))?;
    let past = keys.len();
    if bytes.len() != past.div_ceil(8)
        || (past..bytes.len() * 8).any(|i| bytes[i / 8] >> (i % 8) & 1 == 1)
    {
        return refuse(format!(
            "a reached bitset has {} bytes for {past} sites",
            bytes.len()
        ));
    }
    Ok(keys
        .iter()
        .enumerate()
        .filter(|(i, _)| bytes[i / 8] >> (i % 8) & 1 == 1)
        .map(|(_, key)| (*key).clone())
        .collect())
}

/// Standard base64 with padding, as [`crate::call::base64`] writes it.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = n << 6 | u32::from(digit(c)?);
        }
        n <<= 6 * pad;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}
