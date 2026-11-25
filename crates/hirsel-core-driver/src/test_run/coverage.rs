//! Coverage under `hirsel test --coverage` (`docs/coverage.md`): the raw file
//! a run writes, read against the site map of the executable that wrote it,
//! folded by test and seed into a [`Profile`] and placed in the request's
//! source once, when the request reports; and the `.covmap` a coverage build
//! writes beside each test executable.
use crate::CheckProduct;
use crate::coverage::{Build, Function, Place, Profile, Report, RunCounts, Site, SiteMap, Targets};
use crate::request::hex;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::coverage::{Fragment, RECORD_HEADER, RECORD_MAGIC, Record, Scope, SiteRow};
use hirsel_core_ir::source::SourcePositions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::{io, process::Command, sync::Arc};

pub(super) const ENV: &str = "HIRSEL_COVERAGE";
const END_MARK: u32 = 0x4544_4F4E;

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

/// A finished raw file's seed (none when the run had no `--sim` seed) and
/// whether it recorded a pass; outcome 0, not recorded, reads as failed.
pub(crate) fn header(bytes: &[u8]) -> Result<(Option<u64>, bool), String> {
    records(bytes)?;
    let seed = (word(bytes, 32) == Some(1)).then(|| long(bytes, 40).unwrap());
    Ok((seed, word(bytes, 36) == Some(1)))
}

/// The OR-ed flags of each id a raw file holds.
pub(crate) fn records(bytes: &[u8]) -> Result<BTreeMap<ContentHash, Vec<u8>>, String> {
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

/// The packages of a scope, as the documents name them.
fn scope_names(scope: &Scope) -> BTreeSet<String> {
    scope
        .iter()
        .map(|p| format!("{}@{}", p.name, p.version))
        .collect()
}

/// A site as a run sees it before it is placed.
fn bare(row: &SiteRow, function: &DefPath) -> Site {
    Site {
        arm: row.arm.clone(),
        definition: row
            .decision
            .as_ref()
            .map_or(function, |d| &d.definition)
            .to_string(),
        function: function.to_string(),
        place: Place::default(),
        passed_runs: 0,
        failed_runs: 0,
    }
}

/// Records placed in the request's source: each record's sites, the
/// functions that hold them, and the in-scope functions none holds.
struct Placed {
    records: Vec<(String, Vec<(String, Site)>)>,
    functions: BTreeMap<String, Function>,
    unreferenced: BTreeMap<String, Place>,
}

fn place(product: &CheckProduct, scope: &Scope, records: &[&Record]) -> io::Result<Placed> {
    let functions: BTreeSet<&DefPath> = records.iter().map(|r| &r.function).collect();
    let unreferenced = unreferenced(product, scope, &functions);
    let mut items: BTreeSet<DefPath> = functions.iter().map(|&f| f.clone()).collect();
    items.extend(unreferenced.iter().cloned());
    for row in records.iter().flat_map(|r| &r.sites) {
        items.extend(row.decision.iter().map(|p| p.definition.clone()));
        items.extend(row.target.iter().map(|p| p.definition.clone()));
    }
    let items: Vec<_> = items
        .into_iter()
        .filter(|path| product.headers.get(path).is_some())
        .collect();
    let sources = product.native_sources(&items)?;
    let places = Places(sources.positions());
    Ok(Placed {
        records: records
            .iter()
            .map(|record| {
                let sites = record
                    .sites
                    .iter()
                    .map(|row| (row.key.clone(), places.site(row, &record.function)))
                    .collect();
                (hex(record.id), sites)
            })
            .collect(),
        functions: functions
            .iter()
            .map(|&f| {
                let function = Function {
                    src: crate::law_lock::function_src(product, f),
                    place: places.point(f, 0, true),
                };
                (f.to_string(), function)
            })
            .collect(),
        unreferenced: unreferenced
            .iter()
            .map(|f| {
                let mut place = places.point(f, 0, false);
                place.col = None;
                (f.to_string(), place)
            })
            .collect(),
    })
}

/// Write `map`, the site map of the coverage executable at `output`, to
/// `output.covmap` ("The site map"). Coverage is never a verdict, so a map
/// that cannot be placed or written is reported and the test goes on.
pub(super) fn write_map(
    product: &CheckProduct,
    test: &DefPath,
    options: &crate::NativeOptions,
    map: &Fragment,
) {
    let Some(scope) = &options.coverage else {
        return;
    };
    let mut path = options.output.clone().into_os_string();
    path.push(".covmap");
    let path = PathBuf::from(path);
    let written =
        place(product, scope, &map.records.iter().collect::<Vec<_>>()).and_then(|placed| {
            let map = SiteMap {
                test: test.to_string(),
                build: Build::of(options),
                scope: scope_names(scope),
                functions: placed.functions,
                records: placed.records,
                unreferenced: placed.unreferenced,
            };
            std::fs::write(&path, format!("{}\n", map.json()))
        });
    if let Err(error) = written {
        eprintln!("coverage: {} cannot be written: {error}", path.display());
    }
}

/// What `hirsel test` was asked to do with coverage.
#[derive(Clone, Debug, Default)]
pub(crate) struct Flags {
    pub on: bool,
    pub out: Option<PathBuf>,
    pub base: Option<PathBuf>,
    pub targets: Option<PathBuf>,
    pub runs: bool,
}

impl Flags {
    /// Coverage is on: asked for, or implied by another coverage option.
    pub(crate) fn on(&self) -> bool {
        self.on || self.out.is_some() || self.base.is_some() || self.targets.is_some() || self.runs
    }
}

/// The coverage of one `hirsel test --coverage`: its runs folded as each
/// reports, the map record of each id they hold, and the counts of each
/// JSON row's run.
pub(crate) struct Session {
    build: Build,
    keep_runs: bool,
    profile: Profile,
    records: BTreeMap<ContentHash, Record>,
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

    pub(crate) fn new(options: &crate::NativeOptions, keep_runs: bool) -> Self {
        Self {
            build: Build::of(options),
            keep_runs,
            profile: Profile::default(),
            records: BTreeMap::new(),
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
        let observed = match coverage {
            RunCoverage::Observed(records) => Some(
                records
                    .iter()
                    .flat_map(|(record, flags)| {
                        self.records
                            .entry(record.id)
                            .or_insert_with(|| record.clone());
                        record.sites.iter().zip(flags).map(|(site, &flag)| {
                            (site.key.clone(), bare(site, &record.function), flag != 0)
                        })
                    })
                    .collect(),
            ),
            _ => None,
        };
        let (run, counts) = Profile::of_run(
            &self.build,
            &test.to_string(),
            seed,
            passed,
            observed,
            self.keep_runs,
        );
        self.profile.merge(run).expect("a run is selected once");
        self.rows.insert(row, counts);
        match (counts, coverage) {
            (Some(c), _) => format!(" coverage {} fn, {} arms", c.functions, c.arms),
            (None, RunCoverage::Unobserved(reason)) => format!(" coverage unobserved ({reason})"),
            (None, _) => " coverage unobserved".to_owned(),
        }
    }

    /// The profile, placed in the request's source.
    pub(crate) fn placed(
        self,
        product: &CheckProduct,
        scope: &Scope,
    ) -> io::Result<(Profile, BTreeMap<usize, Option<RunCounts>>)> {
        let Self {
            mut profile,
            records,
            rows,
            ..
        } = self;
        let placed = place(product, scope, &records.values().collect::<Vec<_>>())?;
        for (key, site) in placed.records.into_iter().flat_map(|(_, sites)| sites) {
            if let Some(held) = profile.sites.get_mut(&key) {
                held.place = site.place;
            }
        }
        profile.functions = placed.functions;
        profile.unreferenced = placed.unreferenced;
        profile.scope = scope_names(scope);
        Ok((profile, rows))
    }

    /// The report, with each JSON row's `coverage` set: the functions and
    /// arms its run reached, and the sites its test alone reached. Coverage
    /// is never a verdict, so a profile that cannot be placed or written, or
    /// a base or targets file that cannot be read, is reported and changes
    /// no status.
    pub(crate) fn finish(
        self,
        product: &CheckProduct,
        scope: &Scope,
        rows: &mut [Value],
        flags: &Flags,
    ) -> Option<Report> {
        let (profile, counts) = self
            .placed(product, scope)
            .map_err(|error| eprintln!("coverage: the profile cannot be placed: {error}"))
            .ok()?;
        let base = flags.base.as_deref().and_then(|path| {
            read_profile(path)
                .map_err(|error| eprintln!("coverage: base {}: {error}", path.display()))
                .ok()
        });
        let targets = flags.targets.as_deref().and_then(|path| {
            std::fs::read_to_string(path)
                .map_err(|error| error.to_string())
                .and_then(|text| Targets::read(&text))
                .map_err(|error| eprintln!("coverage: targets {}: {error}", path.display()))
                .ok()
        });
        let report = profile.report(targets.as_ref(), base.as_ref());
        if let Some(path) = &flags.out
            && let Err(error) = std::fs::write(path, format!("{}\n", report.document))
        {
            eprintln!("coverage: {} cannot be written: {error}", path.display());
        }
        let only = profile.only();
        for (index, counts) in counts {
            let row = &mut rows[index];
            let only = row["definition"].as_str().and_then(|test| only.get(test));
            row["coverage"] = counts.map_or(Value::Null, |c| {
                serde_json::json!({"functions": c.functions, "arms": c.arms, "only": only.copied().unwrap_or(0)})
            });
        }
        Some(report)
    }
}

/// The profile a file holds.
fn read_profile(path: &Path) -> Result<Profile, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Profile::from_json(&document).map_err(|e| e.to_string())
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
    /// The line and column of `offset` in `item`, with its file when `file`.
    fn point(&self, item: &DefPath, offset: u32, file: bool) -> Place {
        let mut place = Place::default();
        if let Some((source, line, col)) = self.0.position(item, offset) {
            place.file = file.then(|| relative(&source.path, item));
            place.line = Some(line.into());
            place.col = Some(col.into());
        }
        place
    }

    /// A map row placed ("The site map").
    fn site(&self, row: &SiteRow, function: &DefPath) -> Site {
        let mut site = bare(row, function);
        site.place = match &row.decision {
            None => self.point(function, 0, true),
            Some(decision) => {
                let mut place = self.point(&decision.definition, decision.start, true);
                let end = self.point(&decision.definition, decision.end, false);
                (place.end_line, place.end_col) = (end.line, end.col);
                if let Some(target) = &row.target {
                    let arm = self.point(&target.definition, target.start, false);
                    (place.arm_line, place.arm_col) = (arm.line, arm.col);
                }
                place
            }
        };
        site
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
