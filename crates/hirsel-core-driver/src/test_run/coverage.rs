//! Coverage under `hirsel test --coverage` (`docs/coverage.md`): the raw file
//! a run writes, read against the site map of the executable that wrote it,
//! folded by test and seed into a [`Profile`], and the report and profile
//! document drawn from that.
use crate::CheckProduct;
use crate::request::hex;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::coverage::{Fragment, RECORD_HEADER, RECORD_MAGIC, Record, Scope, SiteRow};
use hirsel_core_ir::source::SourcePositions;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::{io, path::PathBuf, process::Command, sync::Arc};

pub(super) const ENV: &str = "HIRSEL_COVERAGE";
const END_MARK: u32 = 0x4544_4F4E;
/// Rows each section of the text report lists.
const TEXT_ROWS: usize = 20;

/// One run's coverage: each record of its executable's map, with the flags the
/// run set. Positions stay item-relative, so a cached pass is placed against
/// the request's current sources.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum RunCoverage {
    #[default]
    Off,
    Unobserved(String),
    Observed(Vec<(Record, Vec<u8>)>),
}

/// The raw file of one run, read when the run is collected.
pub(super) struct CoverageCapture(Result<(tempfile::TempDir, PathBuf, Arc<Fragment>), String>);

impl CoverageCapture {
    pub(super) fn attach(command: &mut Command, map: Option<&Arc<Fragment>>) -> Self {
        let Some(map) = map else {
            return Self(Err("the build has no site map".into()));
        };
        Self(match tempfile::tempdir() {
            Ok(directory) => {
                let path = directory.path().join("coverage");
                command.env(ENV, &path);
                Ok((directory, path, Arc::clone(map)))
            }
            Err(error) => Err(format!("no directory for the raw file: {error}")),
        })
    }

    pub(super) fn report(&self) -> RunCoverage {
        match &self.0 {
            Err(reason) => RunCoverage::Unobserved(reason.clone()),
            Ok((_, path, map)) => match std::fs::read(path) {
                Ok(bytes) => read(&bytes, map),
                Err(_) => RunCoverage::Unobserved("no raw file".into()),
            },
        }
    }
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn long(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// A raw file ("The raw file") read against `map`. Flags of one id are OR-ed;
/// an id the map does not hold makes the run unobserved, naming it.
pub(crate) fn read(bytes: &[u8], map: &Fragment) -> RunCoverage {
    let mut raw = match records(bytes) {
        Ok(raw) => raw,
        Err(reason) => return RunCoverage::Unobserved(reason),
    };
    let mut observed = Vec::with_capacity(map.records.len());
    for record in &map.records {
        let flags = raw
            .remove(&record.id)
            .unwrap_or_else(|| vec![0; record.sites.len()]);
        if flags.len() != record.sites.len() {
            return RunCoverage::Unobserved(format!(
                "record {} has {} flags and {} sites",
                hex(record.id),
                flags.len(),
                record.sites.len()
            ));
        }
        observed.push((record.clone(), flags));
    }
    match raw.keys().next() {
        Some(id) => RunCoverage::Unobserved(format!("record {} is in no site map", hex(*id))),
        None => RunCoverage::Observed(observed),
    }
}

fn records(bytes: &[u8]) -> Result<BTreeMap<ContentHash, Vec<u8>>, String> {
    if bytes.get(..8) != Some(b"HIRSELCV") || word(bytes, 8) != Some(1) {
        return Err("not a version 1 raw file".into());
    }
    if word(bytes, 48) != Some(END_MARK) {
        return Err("the raw file has no end mark".into());
    }
    let (offset, length) = (long(bytes, 16).unwrap(), long(bytes, 24).unwrap());
    let section = usize::try_from(offset)
        .ok()
        .zip(usize::try_from(length).ok())
        .and_then(|(offset, length)| bytes.get(offset..offset.checked_add(length)?))
        .ok_or("the section lies outside the raw file")?;
    let mut raw = BTreeMap::<ContentHash, Vec<u8>>::new();
    let mut at = 0;
    while at + 8 <= section.len() {
        if section[at..at + 8] == [0; 8] {
            at += 8;
            continue;
        }
        let malformed = || format!("malformed record at section offset {at}");
        if word(section, at) != Some(RECORD_MAGIC) {
            return Err(malformed());
        }
        let n = word(section, at + 4).unwrap() as usize;
        let flags = section
            .get(at + RECORD_HEADER..at + RECORD_HEADER + n)
            .filter(|flags| flags.iter().all(|&flag| flag <= 1))
            .ok_or_else(malformed)?;
        let id = ContentHash(section[at + 8..at + RECORD_HEADER].try_into().unwrap());
        let held = raw.entry(id).or_insert_with(|| vec![0; n]);
        if held.len() != n {
            return Err(malformed());
        }
        for (held, flag) in held.iter_mut().zip(flags) {
            *held |= flag;
        }
        at += (RECORD_HEADER + n).next_multiple_of(8);
    }
    Ok(raw)
}

/// The build a run ran on.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Build {
    pub engine: &'static str,
    pub optimize: bool,
    pub target: String,
}

impl Build {
    pub(crate) fn of(options: &crate::NativeOptions) -> Self {
        Self {
            engine: match options.backend {
                crate::Backend::Cranelift => "cranelift",
                crate::Backend::Llvm => "llvm",
            },
            optimize: options.optimize,
            target: options.target.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct Site {
    row: SiteRow,
    /// The function whose body holds the site.
    function: DefPath,
    passed_runs: u64,
    failed_runs: u64,
}

impl Site {
    fn entry(&self) -> bool {
        self.row.arm.is_none()
    }

    fn reached(&self) -> u64 {
        self.passed_runs + self.failed_runs
    }
}

#[derive(Clone, Debug, Default)]
struct TestRow {
    seeds: BTreeSet<Option<u64>>,
    runs: u64,
    failed_runs: u64,
    reached: BTreeSet<String>,
}

/// What one run reached, for its test row.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunCounts {
    pub functions: usize,
    pub arms: usize,
}

/// A profile folded by test ("The profile"): site counts over observed runs,
/// each test's seeds and reached sites, and every selected run.
#[derive(Clone, Debug, Default)]
pub(crate) struct Profile {
    builds: BTreeSet<Build>,
    sites: BTreeMap<String, Site>,
    tests: BTreeMap<DefPath, TestRow>,
    /// Every selected run, observed or not: test, seed and build.
    selection: BTreeSet<(DefPath, Option<u64>, Build)>,
    runs: u64,
    failed_runs: u64,
    unobserved_runs: u64,
}

impl Profile {
    /// The profile of one selected run, and what it reached when observed.
    pub(crate) fn of_run(
        build: &Build,
        test: &DefPath,
        seed: Option<u64>,
        passed: bool,
        coverage: &RunCoverage,
    ) -> (Self, Option<RunCounts>) {
        let mut profile = Self::default();
        profile.builds.insert(build.clone());
        profile
            .selection
            .insert((test.clone(), seed, build.clone()));
        let row = profile.tests.entry(test.clone()).or_default();
        row.seeds.insert(seed);
        let RunCoverage::Observed(records) = coverage else {
            profile.unobserved_runs = 1;
            return (profile, None);
        };
        for (record, flags) in records {
            for (site, &flag) in record.sites.iter().zip(flags) {
                profile
                    .sites
                    .entry(site.key.clone())
                    .or_insert_with(|| Site {
                        row: site.clone(),
                        function: record.function.clone(),
                        passed_runs: 0,
                        failed_runs: 0,
                    });
                if flag != 0 {
                    row.reached.insert(site.key.clone());
                }
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
        profile.runs = 1;
        profile.failed_runs = u64::from(!passed);
        (profile, Some(counts))
    }

    /// Add `other` ("Merge"): sites match by key, counts add, reached sets
    /// and seeds join. A run in both is refused, since a count cannot be
    /// taken back.
    pub(crate) fn merge(&mut self, other: Self) -> Result<(), String> {
        if let Some((test, seed, build)) = self.selection.intersection(&other.selection).next() {
            let seed = seed.map_or_else(String::new, |seed| format!(" at seed {seed}"));
            return Err(format!(
                "the run of {test}{seed} on {} is in both profiles",
                build.engine
            ));
        }
        self.builds.extend(other.builds);
        self.selection.extend(other.selection);
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

    /// The number of sites each test reached and no other test did.
    pub(crate) fn only(&self) -> BTreeMap<&DefPath, usize> {
        let mut tests = BTreeMap::<&str, usize>::new();
        for key in self.tests.values().flat_map(|row| &row.reached) {
            *tests.entry(key).or_default() += 1;
        }
        self.tests
            .iter()
            .map(|(test, row)| {
                (
                    test,
                    row.reached
                        .iter()
                        .filter(|k| tests[k.as_str()] == 1)
                        .count(),
                )
            })
            .collect()
    }

    /// The first 8 bytes, in hex, of SHA-256 over the sorted (definition,
    /// seed) pair of every selected run, each part framed by its length; a
    /// seed is its eight little-endian bytes, and no seed is empty.
    fn digest(&self) -> String {
        let pairs: BTreeSet<_> = self
            .selection
            .iter()
            .map(|(test, seed, _)| (test.to_string(), *seed))
            .collect();
        let mut hash = Sha256::new();
        for (test, seed) in pairs {
            crate::law_lock::part(&mut hash, test.as_bytes());
            crate::law_lock::part(
                &mut hash,
                &seed.map_or_else(Vec::new, |s| s.to_le_bytes().to_vec()),
            );
        }
        crate::law_lock::short(hash.finalize().as_ref())
    }

    fn selection(&self) -> Value {
        let tests: BTreeSet<_> = self.selection.iter().map(|(test, ..)| test).collect();
        let runs: BTreeSet<_> = self
            .selection
            .iter()
            .map(|(test, seed, _)| (test, seed))
            .collect();
        json!({"tests": tests.len(), "runs": runs.len(), "digest": self.digest()})
    }

    /// The profile document, the report's `coverage` object and its text.
    pub(crate) fn report(&self, product: &CheckProduct, scope: &Scope) -> io::Result<Report> {
        let functions: BTreeSet<&DefPath> = self.sites.values().map(|s| &s.function).collect();
        let unreferenced = unreferenced(product, scope, &functions);
        let mut items: BTreeSet<DefPath> = functions.iter().map(|&f| f.clone()).collect();
        items.extend(unreferenced.iter().cloned());
        for site in self.sites.values() {
            items.extend(site.row.decision.iter().map(|p| p.definition.clone()));
            items.extend(site.row.target.iter().map(|p| p.definition.clone()));
        }
        let items: Vec<_> = items
            .into_iter()
            .filter(|path| product.headers.get(path).is_some())
            .collect();
        let sources = product.native_sources(&items)?;
        let place = Places(sources.positions());
        let builds: Vec<_> = self
            .builds
            .iter()
            .map(|b| json!({"engine": b.engine, "optimize": b.optimize, "target": b.target}))
            .collect();
        let scope: Vec<_> = scope
            .iter()
            .map(|p| format!("{}@{}", p.name, p.version))
            .collect();
        let keys: Vec<&String> = self.sites.keys().collect();
        let function_rows: Vec<Value> = functions
            .iter()
            .map(|&f| {
                let mut row = json!({"definition": f.to_string(), "src": crate::law_lock::function_src(product, f)});
                place.at(&mut row, f, 0, "line", "col", true);
                row
            })
            .collect();
        let unreferenced: Vec<Value> = unreferenced
            .iter()
            .map(|f| {
                let mut row = json!({"definition": f.to_string()});
                place.at(&mut row, f, 0, "line", "", true);
                row
            })
            .collect();
        let profile = json!({
            "hirsel_coverage": 1,
            "builds": builds,
            "scope": scope,
            "selection": self.selection(),
            "runs": self.runs, "failed_runs": self.failed_runs, "unobserved_runs": self.unobserved_runs,
            "functions": function_rows,
            "sites": self.sites.iter().map(|(key, site)| {
                let mut row = place.site(key, site);
                row["passed_runs"] = site.passed_runs.into();
                row["failed_runs"] = site.failed_runs.into();
                row
            }).collect::<Vec<_>>(),
            "unreferenced": unreferenced,
            "tests": self.tests.iter().map(|(test, row)| json!({
                "definition": test.to_string(),
                "seeds": seeds(&row.seeds),
                "runs": row.runs, "failed_runs": row.failed_runs,
                "reached": crate::call::base64(&bitset(&keys, &row.reached)),
            })).collect::<Vec<_>>(),
        });
        Ok(self.coverage(profile, &place))
    }

    fn coverage(&self, profile: Value, place: &Places<'_>) -> Report {
        let entered = |function: &DefPath| {
            self.sites
                .get(&format!("{function}#e"))
                .map_or(0, Site::reached)
        };
        let count = |entry: bool, filter: &dyn Fn(&Site) -> bool| {
            let sites = self
                .sites
                .values()
                .filter(|s| s.entry() == entry && filter(s));
            let (mut reached, mut total) = (0, 0);
            for site in sites {
                total += 1;
                reached += u64::from(site.reached() > 0);
            }
            json!({"reached": reached, "total": total})
        };
        // By function, its arms reached and in all.
        let mut arms = BTreeMap::<String, (u64, u64)>::new();
        for site in self.sites.values().filter(|s| !s.entry()) {
            let held = arms.entry(site.function.to_string()).or_default();
            held.0 += u64::from(site.reached() > 0);
            held.1 += 1;
        }
        let file = |site: &Site| place.file(site);
        let files: BTreeSet<String> = self.sites.values().filter_map(file).collect();
        let files: Vec<Value> = files
            .iter()
            .map(|name| {
                let here = |s: &Site| file(s).as_deref() == Some(name);
                json!({"file": name, "functions": count(true, &here), "arms": count(false, &here)})
            })
            .collect();
        let functions: Vec<Value> = profile["functions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let mut row = row.clone();
                let definition = row["definition"].as_str().unwrap().to_owned();
                row.as_object_mut().unwrap().remove("col");
                row["entered_runs"] = self
                    .sites
                    .get(&format!("{definition}#e"))
                    .map_or(0, Site::reached)
                    .into();
                let (reached, total) = arms.get(&definition).copied().unwrap_or_default();
                row["arms"] = json!({"reached": reached, "total": total});
                row
            })
            .collect();
        let mut uncovered = Vec::new();
        for (key, site) in self.sites.iter().filter(|(_, s)| s.reached() == 0) {
            let entered_runs = entered(&site.function);
            let mut row = place.site(key, site);
            if site.entry() {
                row["state"] = "never entered".into();
                row["arms"] = arms
                    .get(&site.function.to_string())
                    .map_or(0, |a| a.1)
                    .into();
            } else if entered_runs == 0 {
                continue;
            } else {
                // The arms of one decision are the keys up to their last `:`.
                let decision = &key[..=key.rfind(':').unwrap()];
                let taken = self
                    .sites
                    .range::<str, _>((
                        std::ops::Bound::Included(decision),
                        std::ops::Bound::Unbounded,
                    ))
                    .take_while(|(k, _)| k.starts_with(decision))
                    .any(|(_, s)| s.reached() > 0);
                row["state"] = if taken {
                    "other arm taken"
                } else {
                    "decision not reached"
                }
                .into();
                row["entered_runs"] = entered_runs.into();
            }
            uncovered.push((entered_runs, key, row));
        }
        uncovered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let totals = json!({
            "functions": count(true, &|_| true),
            "arms": count(false, &|_| true),
            "unreferenced": profile["unreferenced"].as_array().unwrap().len(),
        });
        let text = self.text(&profile, &totals, &files, &uncovered);
        let coverage = json!({
            "schema": 1,
            "builds": profile["builds"], "scope": profile["scope"], "selection": profile["selection"],
            "runs": self.runs, "failed_runs": self.failed_runs, "unobserved_runs": self.unobserved_runs,
            "totals": totals,
            "files": files,
            "functions": functions,
            "uncovered": uncovered.into_iter().map(|(_, _, row)| row).collect::<Vec<_>>(),
            "unreferenced": profile["unreferenced"],
        });
        Report {
            profile,
            coverage,
            text,
        }
    }

    fn text(
        &self,
        profile: &Value,
        totals: &Value,
        files: &[Value],
        uncovered: &[(u64, &String, Value)],
    ) -> String {
        let fraction = |v: &Value| format!("{}/{}", v["reached"], v["total"]);
        let mut text = String::new();
        let builds: Vec<String> = self
            .builds
            .iter()
            .map(|b| {
                format!(
                    "{} {}",
                    b.engine,
                    if b.optimize { "release" } else { "dev" }
                )
            })
            .collect();
        let scope: Vec<&str> = profile["scope"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let selection = &profile["selection"];
        writeln!(
            text,
            "coverage  {}  {}  {} tests, {} runs",
            scope.join(" "),
            builds.join(", "),
            selection["tests"],
            selection["runs"]
        )
        .unwrap();
        writeln!(
            text,
            "  functions {}   arms {}   {} unreferenced",
            fraction(&totals["functions"]),
            fraction(&totals["arms"]),
            totals["unreferenced"]
        )
        .unwrap();
        if self.unobserved_runs > 0 {
            writeln!(text, "  {} runs unobserved", self.unobserved_runs).unwrap();
        }
        for file in files {
            writeln!(
                text,
                "  {:<24} functions {}   arms {}",
                file["file"].as_str().unwrap(),
                fraction(&file["functions"]),
                fraction(&file["arms"])
            )
            .unwrap();
        }
        let (never, arms): (Vec<_>, Vec<_>) = uncovered
            .iter()
            .partition(|(_, _, row)| row["kind"] == "entry");
        let at = |row: &Value, col: bool| {
            let mut at = format!("{}:{}", row["file"].as_str().unwrap_or("?"), row["line"]);
            if col {
                write!(at, ":{}", row["col"]).unwrap();
            }
            at
        };
        if !never.is_empty() {
            writeln!(
                text,
                "never entered ({} of {}; --json lists all)",
                never.len(),
                totals["functions"]["total"]
            )
            .unwrap();
            for (_, _, row) in never.iter().take(TEXT_ROWS) {
                writeln!(
                    text,
                    "  {:<36} {}",
                    row["definition"].as_str().unwrap(),
                    at(row, false)
                )
                .unwrap();
            }
            more(&mut text, never.len());
        }
        if !arms.is_empty() {
            writeln!(text, "entered, arm not reached (most-entered first)").unwrap();
            for (entered, _, row) in arms.iter().take(TEXT_ROWS) {
                writeln!(
                    text,
                    "  {}   {}   {}   {}   entered by {entered} runs",
                    row["definition"].as_str().unwrap(),
                    at(row, true),
                    row["arm"].as_str().unwrap(),
                    row["state"].as_str().unwrap()
                )
                .unwrap();
            }
            more(&mut text, arms.len());
        }
        let only = self.only();
        for (test, row) in &self.tests {
            let reached = row.reached.iter().map(|k| &self.sites[k]);
            let (functions, arms) =
                reached.fold(
                    (0, 0),
                    |(f, a), s| if s.entry() { (f + 1, a) } else { (f, a + 1) },
                );
            writeln!(
                text,
                "  {test}  coverage {functions} fn, {arms} arms, {} only",
                only[test]
            )
            .unwrap();
        }
        text
    }
}

fn more(text: &mut String, rows: usize) {
    if rows > TEXT_ROWS {
        writeln!(text, "  ... {} more", rows - TEXT_ROWS).unwrap();
    }
}

/// The coverage of one `hirsel test --coverage`: its runs folded as each
/// reports, and the counts of each JSON row's run.
pub(crate) struct Session {
    build: Build,
    profile: Profile,
    rows: BTreeMap<usize, Option<RunCounts>>,
}

impl Session {
    /// The packages given as source in the request ("Scope").
    pub(crate) fn scope(product: &CheckProduct) -> Scope {
        product
            .files
            .iter()
            .map(|file| product.index.module_path(file.module).package)
            .collect()
    }

    pub(crate) fn new(options: &crate::NativeOptions) -> Self {
        Self {
            build: Build::of(options),
            profile: Profile::default(),
            rows: BTreeMap::new(),
        }
    }

    /// Fold the run of JSON row `row`; what its text line gains.
    pub(crate) fn run(
        &mut self,
        row: usize,
        test: &DefPath,
        seed: Option<u64>,
        passed: bool,
        coverage: &RunCoverage,
    ) -> String {
        let (run, counts) = Profile::of_run(&self.build, test, seed, passed, coverage);
        self.profile.merge(run).expect("a run is selected once");
        self.rows.insert(row, counts);
        match (counts, coverage) {
            (Some(c), _) => format!(" coverage {} fn, {} arms", c.functions, c.arms),
            (None, RunCoverage::Unobserved(reason)) => format!(" coverage unobserved ({reason})"),
            (None, _) => " coverage unobserved".to_owned(),
        }
    }

    /// The report, with each JSON row's `coverage` set: the functions and
    /// arms its run reached, and the sites its test alone reached. Coverage
    /// is never a verdict, so a profile that cannot be placed or written is
    /// reported and changes no status.
    pub(crate) fn finish(
        self,
        product: &CheckProduct,
        scope: &Scope,
        rows: &mut [Value],
        out: Option<&std::path::Path>,
    ) -> Option<Report> {
        let report = self
            .profile
            .report(product, scope)
            .map_err(|error| eprintln!("coverage: the profile cannot be placed: {error}"))
            .ok()?;
        if let Some(path) = out
            && let Err(error) = std::fs::write(path, format!("{}\n", report.profile))
        {
            eprintln!("coverage: {} cannot be written: {error}", path.display());
        }
        let only: BTreeMap<String, usize> = self
            .profile
            .only()
            .into_iter()
            .map(|(test, n)| (test.to_string(), n))
            .collect();
        for (index, counts) in self.rows {
            let row = &mut rows[index];
            let only = row["definition"].as_str().and_then(|test| only.get(test));
            row["coverage"] = counts.map_or(Value::Null, |c| {
                json!({"functions": c.functions, "arms": c.arms, "only": only.copied().unwrap_or(0)})
            });
        }
        Some(report)
    }
}

/// The report of a profile.
pub(crate) struct Report {
    /// The document `--coverage-out` writes.
    pub profile: Value,
    /// The `--json` document's top-level `coverage`.
    pub coverage: Value,
    pub text: String,
}

/// A source file's path relative to its package's root. A file no manifest
/// named is called by its module path (`app@1/main.hirsel`), and loses the
/// package part.
fn relative(path: &str, item: &DefPath) -> String {
    let package = &item.0.package;
    let prefix = format!("{}@{}/", package.name, package.version);
    path.strip_prefix(&prefix).unwrap_or(path).to_owned()
}

/// Sites placed in the request's source.
struct Places<'a>(SourcePositions<'a>);

impl Places<'_> {
    /// Set `line`/`col` keys (and `file` when `file` is true) on `row`.
    fn at(&self, row: &mut Value, item: &DefPath, offset: u32, line: &str, col: &str, file: bool) {
        if let Some((source, l, c)) = self.0.position(item, offset) {
            if file {
                row["file"] = relative(&source.path, item).into();
            }
            row[line] = l.into();
            if !col.is_empty() {
                row[col] = c.into();
            }
        }
    }

    fn file(&self, site: &Site) -> Option<String> {
        let (item, offset) = match &site.row.decision {
            Some(decision) => (&decision.definition, decision.start),
            None => (&site.function, 0),
        };
        self.0
            .position(item, offset)
            .map(|(source, ..)| relative(&source.path, item))
    }

    /// A site row with the map's fields for its key ("The site map").
    fn site(&self, key: &str, site: &Site) -> Value {
        let mut row = json!({"site": key, "kind": if site.entry() { "entry" } else { "arm" }});
        match &site.row.decision {
            None => {
                row["definition"] = site.function.to_string().into();
                self.at(&mut row, &site.function, 0, "line", "col", true);
            }
            Some(decision) => {
                row["arm"] = site.row.arm.clone().into();
                row["definition"] = decision.definition.to_string().into();
                self.at(
                    &mut row,
                    &decision.definition,
                    decision.start,
                    "line",
                    "col",
                    true,
                );
                self.at(
                    &mut row,
                    &decision.definition,
                    decision.end,
                    "end_line",
                    "end_col",
                    false,
                );
                if let Some(target) = &site.row.target {
                    self.at(
                        &mut row,
                        &target.definition,
                        target.start,
                        "arm_line",
                        "arm_col",
                        false,
                    );
                }
            }
        }
        row
    }
}

/// The in-scope source functions in no reached body.
fn unreferenced(
    product: &CheckProduct,
    scope: &Scope,
    reached: &BTreeSet<&DefPath>,
) -> Vec<DefPath> {
    let sources: BTreeSet<_> = product.files.iter().map(|file| file.file).collect();
    let mut paths: Vec<DefPath> = product
        .index
        .definitions()
        .filter(|(_, d)| d.kind == hirsel_core_resolve::DefinitionKind::Function)
        .map(|(id, _)| product.index.definition_path(id))
        .filter(|path| scope.contains(&path.0.package) && !reached.contains(path))
        .filter(|path| {
            product
                .headers
                .get(path)
                .is_some_and(|header| sources.contains(&header.anchor.1))
        })
        .collect();
    paths.sort();
    paths
}

/// Seeds as sorted, merged, inclusive ranges; null for a test run without one.
fn seeds(seeds: &BTreeSet<Option<u64>>) -> Value {
    let mut ranges: Vec<[u64; 2]> = Vec::new();
    for &seed in seeds.iter().flatten() {
        match ranges.last_mut() {
            Some(range) if range[1].checked_add(1) == Some(seed) => range[1] = seed,
            _ => ranges.push([seed, seed]),
        }
    }
    if ranges.is_empty() {
        Value::Null
    } else {
        json!(ranges)
    }
}

/// Bit `i` set when `keys[i]` is reached, least significant bit first.
fn bitset(keys: &[&String], reached: &BTreeSet<String>) -> Vec<u8> {
    let mut bytes = vec![0u8; keys.len().div_ceil(8)];
    for (i, key) in keys.iter().enumerate() {
        if reached.contains(*key) {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    bytes
}
