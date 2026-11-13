/// Deterministic providers selected explicitly by a semantic fixture, never a host grant.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// A disjoint part of H's fault campaign; every part still checks the full clean run.
    pub heap_campaign_partition: Option<hirsel_heapcheck::CampaignPartition>,
    /// A whole campaign (no partition) whose clean run has more failure-capable
    /// sites than this is not run here: the clean run is checked, and its sites
    /// are left to `heap_campaign_partition` parts (`Outcome::Parted`).
    pub heap_campaign_sites_max: Option<u64>,
    /// Where each heap+inject configuration records what its runs cost
    /// (tools/corpus sizes campaign parts from them).
    pub heap_measures: Option<HeapMeasures>,
    /// Closed CPU batches use the same explicit grant in every engine.
    pub cpu: Option<hirsel_run_schema::CpuGrant>,
    pub tasks_free: bool,
    pub args: Vec<String>,
    /// Plain bytes followed by EOF, shared with SimulationConfig. No readiness script.
    pub stdin: Vec<u8>,
    pub files: std::collections::BTreeMap<String, Vec<u8>>,
    pub mounts: std::collections::BTreeMap<String, hirsel_run_schema::Access>,
    /// Simulation-only regular-file readiness, shared with SimulationConfig.
    pub file_reads: std::collections::BTreeMap<String, hirsel_run_schema::FileReadScript>,
    /// Ordered snapshot-only file inputs; byte counts are checked before consumption.
    pub file_faults: hirsel_run_schema::FileFaults,
    /// The negative output-grant fixture, shared by every engine.
    pub deny_output: bool,
    pub monotonic: Option<u64>,
    pub net: Option<hirsel_run_schema::NetGrant>,
    pub process: Option<hirsel_run_schema::ProcessGrant>,
    /// The `io.signals` grant, with its script under `signals.script@1`.
    pub signals: Option<hirsel_run_schema::SignalsGrant>,
    /// The root Env's snapshot: each granted name and its value, null when unset.
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// A corpus program's raised interpreter step budget (`// steps:`); H's
    /// instruction limit rises by the same factor. None keeps both defaults.
    pub steps: Option<u64>,
    /// `--trace-roots` (S-14): native launches on the diagnostic archive write every live
    /// `Shared` root's count at every scheduling point into `Observation::roots`, to be
    /// compared with the interpreter's trace.
    pub trace_roots: bool,
    /// `--migrate` (design §9 W1): native launches run on several compute workers and move
    /// every task to another worker at every suspension (`HIRSEL_RT_MIGRATE`), and native
    /// resumable task bodies save their compact frame at every boundary.
    pub migrate: bool,
}

/// What one heap+inject configuration's runs of a fixture cost.
#[derive(Clone, Debug, PartialEq)]
pub struct HeapMeasure {
    pub configuration: String,
    /// The clean run's failure-capable sites.
    pub sites: u64,
    /// The clean run's wall time, when it ran alone (under a site bound).
    pub clean: Option<std::time::Duration>,
    /// The campaign, when one ran: its partition, its wall time (the clean
    /// run's interleaved steps included) and each perturbation's own time.
    pub campaign: Option<HeapCampaignMeasure>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeapCampaignMeasure {
    pub partition: Option<hirsel_heapcheck::CampaignPartition>,
    pub wall: std::time::Duration,
    pub timing: hirsel_heapcheck::CampaignTiming,
}

/// A run's heap measures, shared by the fixtures of its configurations.
pub type HeapMeasures = std::sync::Arc<std::sync::Mutex<Vec<HeapMeasure>>>;

impl Fixture {}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            heap_campaign_partition: None,
            heap_campaign_sites_max: None,
            heap_measures: None,
            cpu: None,
            tasks_free: false,
            args: vec![],
            stdin: vec![],
            files: Default::default(),
            mounts: hirsel_engine::SimulationConfig::default().mounts,
            file_reads: Default::default(),
            file_faults: Default::default(),
            deny_output: false,
            monotonic: None,
            net: None,
            process: None,
            signals: None,
            env: None,
            steps: None,
            trace_roots: false,
            migrate: false,
        }
    }
}

impl From<&Fixture> for hirsel_engine::SimulationConfig {
    fn from(fixture: &Fixture) -> Self {
        let mut config = Self {
            cpu: fixture.cpu.clone(),
            tasks_free: fixture.tasks_free,
            args: fixture.args.clone(),
            stdin: fixture.stdin.clone(),
            files: fixture.files.clone(),
            mounts: fixture.mounts.clone(),
            file_reads: fixture.file_reads.clone(),
            file_faults: fixture.file_faults.clone(),
            net: fixture.net.clone(),
            process: fixture.process.clone(),
            signals: fixture.signals.clone(),
            env: fixture.env.clone().unwrap_or_default(),
            monotonic: fixture.monotonic.unwrap_or(0),
            trace_roots: fixture.trace_roots,
            ..Default::default()
        };
        if fixture.deny_output {
            config.projections.remove("stdout");
            config.projections.remove("stderr");
        }
        if fixture.monotonic.is_some() {
            config.grants.insert("MonotonicClock".into());
            config.projections.insert("monotonic_clock".into());
        }
        config
    }
}
