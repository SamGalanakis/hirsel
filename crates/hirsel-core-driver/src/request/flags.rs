//! Decode native and checking arguments before opening inputs or running a tool.
use crate::Backend;
use crate::cli::CompilerOption as Opt;
use crate::installation::Flavour;
use hirsel_core_ids::{ContentHash, DefPath};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
pub(super) struct CompilerFlags {
    pub(super) libraries: Option<PathBuf>,
    pub(super) store: Option<PathBuf>,
    pub(super) manifest: Option<PathBuf>,
    pub(super) metrics: Option<PathBuf>,
    pub(super) profile_front: bool,
    pub(super) image_path: Option<PathBuf>,
    pub(super) install_root: Option<PathBuf>,
    pub(super) exec_helper: Option<PathBuf>,
    pub(super) link_kit: Option<PathBuf>,
    pub(super) core_product: Option<PathBuf>,
    pub(super) source_core: bool,
    pub(super) source_std: bool,
    pub(super) product_name: Option<DefPath>,
    pub(super) entry: Option<DefPath>,
    pub(super) recipe_root: Option<ContentHash>,
    pub(super) code_store: Option<PathBuf>,
    pub(super) release_manifest: Option<PathBuf>,
    pub(super) n_place: Option<u64>,
    pub(super) place_timeout: Duration,
    pub(super) test_filter: Option<String>,
    pub(super) test_exact: Vec<DefPath>,
    pub(super) test_jobs: Option<std::num::NonZeroUsize>,
    pub(super) test_timeout: Option<Duration>,
    pub(super) test_sim: bool,
    pub(super) corpus_path: Option<PathBuf>,
    pub(super) test_seeds: Option<u64>,
    pub(super) test_seed: Option<u64>,
    pub(super) backend: Option<Backend>,
    pub(super) release: bool,
    pub(super) debug_lines: Option<bool>,
    pub(super) runtime: Option<PathBuf>,
    pub(super) explicit_runtime: bool,
    pub(super) explicit_target: bool,
    pub(super) logical_charge_report: Option<PathBuf>,
    pub(super) output_path: Option<PathBuf>,
    pub(super) root_limit: Option<u64>,
    pub(super) emulator: Option<PathBuf>,
    pub(super) sysroot: Option<PathBuf>,
    pub(super) target: String,
    pub(super) cc: Option<PathBuf>,
    pub(super) recording_build: bool,
    pub(super) additional_runtimes: BTreeMap<Flavour, PathBuf>,
    pub(super) record_path: Option<String>,
    pub(super) replay_outputs: Option<String>,
    pub(super) replay_tape: Option<String>,
    pub(super) test_replay: bool,
    pub(super) test_laws: bool,
    pub(super) test_write_laws: bool,
    pub(super) law_baseline: Option<PathBuf>,
    pub(super) test_rerun: bool,
    pub(super) test_cache: PathBuf,
    pub(super) coverage: crate::test_run::Flags,
    pub(super) seed_base: Option<u64>,
    pub(super) expect_verdict: Option<String>,
    pub(super) diagnose: bool,
    pub(super) grants: crate::package_dir::Grants,
    pub(super) json: bool,
}

impl CompilerFlags {
    pub(super) fn read(
        options: Vec<Opt>,
        source_core: bool,
        test_cache: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut flags = Self {
            source_core,
            place_timeout: Duration::from_secs(10),
            target: crate::host::HOST_TARGET.triple().to_owned(),
            test_cache,
            ..Self::default()
        };
        for option in options {
            match option {
                Opt::Emit(_) => unreachable!("request validates and removes --emit"),
                Opt::Json => flags.json = true,
                Opt::Recipe(value) => {
                    flags.recipe_root = Some(crate::native::recipe_build::parse_root(&value)?)
                }
                Opt::CodeStore(value) => flags.code_store = Some(PathBuf::from(value)),
                Opt::ReleaseManifest(value) => flags.release_manifest = Some(PathBuf::from(value)),
                Opt::NPlace(value) => flags.n_place = Some(value.parse::<u64>()?),
                Opt::PlaceTimeout(value) => {
                    flags.place_timeout = Duration::from_secs(value.parse::<u64>()?);
                    if flags.place_timeout.is_zero() {
                        return Err("--place-timeout must be positive".into());
                    }
                }
                Opt::Recording => flags.recording_build = true,
                Opt::EmbedRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::EMBEDDED, PathBuf::from(value));
                }
                Opt::ExecHelper(value) => flags.exec_helper = Some(PathBuf::from(value)),
                Opt::RecordingRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::RECORDING, PathBuf::from(value));
                }
                Opt::ProfRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::PROF, PathBuf::from(value));
                }
                Opt::ProfRecordingRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::PROF_RECORDING, PathBuf::from(value));
                }
                Opt::ObservedRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::OBSERVED, PathBuf::from(value));
                }
                Opt::ObservedRecordingRuntime(value) => {
                    flags
                        .additional_runtimes
                        .insert(Flavour::OBSERVED_RECORDING, PathBuf::from(value));
                }
                Opt::Record(value) => flags.record_path = Some(value),
                Opt::ReplayOutputs(value) => flags.replay_outputs = Some(value),
                Opt::Tape(value) => flags.replay_tape = Some(value),
                Opt::ReplayUnit(_) => {
                    return Err("replay --unit needs --state DIR with --tape".into());
                }
                Opt::Replay => flags.test_replay = true,
                Opt::Laws => flags.test_laws = true,
                Opt::WriteLaws => flags.test_write_laws = true,
                Opt::LawsBaseline(value) => flags.law_baseline = Some(PathBuf::from(value)),
                Opt::Rerun => flags.test_rerun = true,
                Opt::LinkOnly => {}
                Opt::TestCache(value) => flags.test_cache = PathBuf::from(value),
                Opt::Coverage => flags.coverage.on = true,
                Opt::CoverageOut(value) => flags.coverage.out = Some(PathBuf::from(value)),
                Opt::CoverageBase(value) => flags.coverage.base = Some(PathBuf::from(value)),
                Opt::CoverageTargets(value) => flags.coverage.targets = Some(PathBuf::from(value)),
                Opt::CoverageRuns => flags.coverage.runs = true,
                Opt::SeedBase(value) => {
                    flags.seed_base = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| "--seed-base expects a u64")?,
                    );
                }
                Opt::Sim => flags.test_sim = true,
                Opt::PropertyCorpus(value) => flags.corpus_path = Some(PathBuf::from(value)),
                Opt::SimSeeds(value) => {
                    flags.test_seeds = Some(
                        value
                            .parse::<std::num::NonZeroU64>()
                            .map_err(|_| "--seeds expects a positive count")?
                            .get(),
                    );
                }
                Opt::SimSeed(value) => {
                    flags.test_seed =
                        Some(value.parse::<u64>().map_err(|_| "--seed expects a u64")?);
                }
                Opt::ExpectVerdict(value) => flags.expect_verdict = Some(value),
                Opt::Diagnose => flags.diagnose = true,
                Opt::Install(value) => flags.install_root = Some(PathBuf::from(value)),
                Opt::LinkKit(value) => flags.link_kit = Some(PathBuf::from(value)),
                Opt::SourceCore => flags.source_core = true,
                Opt::SourceStd => flags.source_std = true,
                Opt::Name(value) => {
                    flags.product_name =
                        Some(DefPath::from_full(&value).ok_or("invalid --name definition path")?)
                }
                Opt::CoreProduct(value) => flags.core_product = Some(PathBuf::from(value)),
                Opt::Dir(value) => flags
                    .grants
                    .directories
                    .push(hirsel_core_ids::launch::dir_slot(&value)?),
                Opt::Grant(value) => flags.grants.grant(&value)?,
                Opt::Filter(value) => flags.test_filter = Some(value),
                Opt::TestExact(value) => flags.test_exact.push(
                    DefPath::from_full(&value).ok_or("--exact expects a full definition path")?,
                ),
                Opt::TestJobs(value) => {
                    flags.test_jobs = Some(
                        value
                            .parse()
                            .map_err(|_| "-j expects a positive job count")?,
                    );
                }
                Opt::TestTimeout(value) => {
                    let seconds: u64 = value.parse()?;
                    if seconds == 0 {
                        return Err("test timeout must be positive".into());
                    }
                    flags.test_timeout = Some(std::time::Duration::from_secs(seconds));
                }
                Opt::Release => flags.release = true,
                Opt::DebugLines(value) => {
                    flags.debug_lines = Some(match value.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err("--debug-lines expects on or off".into()),
                    })
                }
                Opt::Backend(value) => flags.backend = Some(value.parse::<Backend>()?),
                Opt::Runtime(value) => {
                    flags.explicit_runtime = true;
                    flags.runtime = Some(PathBuf::from(value));
                }
                Opt::LogicalChargeReport(value) => {
                    flags.logical_charge_report = Some(PathBuf::from(value));
                }
                Opt::Cc(value) => flags.cc = Some(PathBuf::from(value)),
                Opt::Target(value) => {
                    flags.explicit_target = true;
                    flags.target = value;
                }
                Opt::Emulator(value) => flags.emulator = Some(PathBuf::from(value)),
                Opt::Sysroot(value) => flags.sysroot = Some(PathBuf::from(value)),
                Opt::Limit(value) => flags.root_limit = Some(value.parse()?),
                Opt::Output(value) => flags.output_path = Some(PathBuf::from(value)),
                Opt::Entry(value) => {
                    flags.entry = Some(DefPath::from_full(&value).ok_or("invalid entry path")?)
                }
                Opt::Libraries(value) => flags.libraries = Some(PathBuf::from(value)),
                Opt::Store(value) => flags.store = Some(PathBuf::from(value)),
                Opt::Inputs(value) => flags.manifest = Some(PathBuf::from(value)),
                Opt::Metrics(value) => flags.metrics = Some(PathBuf::from(value)),
                Opt::ProfileFront => flags.profile_front = true,
                // Routed before the request runs (`resident::submit`).
                Opt::Resident => {}
                Opt::Image(value) => flags.image_path = Some(PathBuf::from(value)),
                Opt::AffectedTests(_)
                | Opt::Dependents(_)
                | Opt::FleetHosts(_)
                | Opt::FleetPartitions(_)
                | Opt::FleetPlacement
                | Opt::FleetPlaces(_)
                | Opt::FleetRounds(_)
                | Opt::ForeignAbi(_)
                | Opt::ForeignHeader(_)
                | Opt::Frozen
                | Opt::HostState(_)
                | Opt::InspectCore
                | Opt::InspectFilter(_)
                | Opt::InspectLibrary
                | Opt::InspectModule(_)
                | Opt::InspectStd
                | Opt::Interface
                | Opt::Locked
                | Opt::PackageStore(_)
                | Opt::ProfFrequency(_)
                | Opt::ProfKernel
                | Opt::ProfMem(_)
                | Opt::ProfOut(_)
                | Opt::ProfPmu(_)
                | Opt::ProfStackBytes(_)
                | Opt::ProfTasks
                | Opt::Transitive
                | Opt::Unused => unreachable!("CLI early command option"),
            }
        }
        if flags.test_filter.is_some() && !flags.test_exact.is_empty() {
            return Err("test: --filter and --exact cannot be combined".into());
        }
        if flags.law_baseline.is_some() {
            for (narrowed, option) in [
                (flags.test_filter.is_some(), "--filter"),
                (!flags.test_exact.is_empty(), "--exact"),
                (flags.test_seed.is_some(), "--seed"),
                (flags.test_sim, "--sim"),
                (flags.test_seeds.is_some(), "--seeds"),
                (flags.corpus_path.is_some(), "--property-corpus"),
                (flags.test_replay, "--replay"),
            ] {
                if narrowed {
                    return Err(format!(
                        "test: --laws-baseline compares a full run; drop {option}"
                    )
                    .into());
                }
            }
        }
        Ok(flags)
    }
}
