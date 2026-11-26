//! Typed arguments shared by commands with the same interpreter.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddOption {
    AddDryRun,
    AddRoot(String),
    AddUpdate,
    AddWorkspace,
    Frozen,
    Json,
    Locked,
    PackageStore(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachOption {
    ProfDebug(String),
    ProfDuration(String),
    ProfFrequency(String),
    ProfOut(String),
    ProfPmu(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileQueryOption {
    Json,
    ProfBy(String),
    ProfDepth(String),
    ProfEvent(String),
    ProfFilter(String),
    ProfGroup(String),
    ProfMinDeltaPct(String),
    ProfRows(String),
    ProfTimeout(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileExportOption {
    ProfFormat(String),
    ProfOut(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageOption {
    CoverageMap(String),
    CoverageTest(String),
    Json,
    MergeBase(String),
    MergeTargets(String),
    Output(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeanOption {
    CoreIr,
    Inputs(String),
    Output(String),
    Store(String),
    Target(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostOption {
    Admission(String),
    HostState(String),
    IdleMs(String),
    Lifecycle(String),
    MaxPlaces(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherOption {
    Admission(String),
    Control(String),
    HostState(String),
    IdleMs(String),
    Lifecycle(String),
    MaxPlaces(String),
    OperatorListen(String),
    PlatformHost(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterOption {
    CodeStore(String),
    FrontHttp(String),
    FrontStream(String),
    Grant(String),
    GroupName(String),
    GroupStore(String),
    HostMemory(String),
    HostState(String),
    Listen(String),
    PackageRoot(String),
    Record(String),
    RequestId(String),
    SourceDir(String),
    TapeRetention(String),
    Tenant(String),
    Trusted,
    Workdir(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeployOption {
    ApproveGrant(String),
    CodeStore(String),
    Grant(String),
    GroupName(String),
    HeldAll(String),
    HeldEntry(String),
    HostState(String),
    RequestId(String),
    RequestStatus(String),
    SourceDir(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecideOption {
    ApproveGrant(String),
    HeldAll(String),
    HeldEntry(String),
    HostState(String),
    Refuse,
    RequestId(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallOption {
    CallStream(String),
    CallTimeout(String),
    NPlace(String),
    PlaceTimeout(String),
    Url(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevOption {
    Backend(String),
    Dir(String),
    Fresh,
    Grant(String),
    Libraries(String),
    Limit(String),
    Listen(String),
    NPlace(String),
    Runtime(String),
    SourceCore,
    SourceStd,
    Store(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOption {
    DryRunExe(String),
    GroupName(String),
    HostState(String),
    RecordedExecutable(String),
    Store(String),
    StoreRelease(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServeOption {
    MaxMemory(String),
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdkOption {
    Backend(String),
    Cc(String),
    CoreProduct(String),
    DebugLines(String),
    Entry(String),
    Frozen,
    Inputs(String),
    Libraries(String),
    Limit(String),
    Locked,
    NPlace(String),
    Output(String),
    PackageStore(String),
    PlaceTimeout(String),
    Release,
    Runtime(String),
    SourceCore,
    SourceStd,
    Store(String),
    Target(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapeOption {
    IncludeCredentials,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewOption {
    Lib,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FmtOption {
    FmtCheck,
    FmtStdin,
    Signatures,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplainOption {
    Json,
    ListRules,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerOption {
    AffectedTests(String),
    Backend(String),
    Cc(String),
    CodeStore(String),
    CoreProduct(String),
    Coverage,
    CoverageBase(String),
    CoverageOut(String),
    CoverageRuns,
    CoverageTargets(String),
    DebugLines(String),
    Dependents(String),
    Diagnose,
    Dir(String),
    Emulator(String),
    Entry(String),
    ExpectVerdict(String),
    Filter(String),
    FleetHosts(String),
    FleetPartitions(String),
    FleetPlacement,
    FleetPlaces(String),
    FleetRounds(String),
    ForeignAbi(String),
    ForeignHeader(String),
    Frozen,
    Grant(String),
    HostState(String),
    Image(String),
    Inputs(String),
    InspectCore,
    InspectFilter(String),
    InspectLibrary,
    InspectModule(String),
    InspectStd,
    Install(String),
    Interface,
    Json,
    Laws,
    LawsBaseline(String),
    Libraries(String),
    Limit(String),
    LinkKit(String),
    LinkOnly,
    Locked,
    LogicalChargeReport(String),
    Metrics(String),
    NPlace(String),
    Name(String),
    ObservedRecordingRuntime(String),
    ObservedRuntime(String),
    Output(String),
    PackageStore(String),
    PlaceTimeout(String),
    ProfFrequency(String),
    ProfMem(hirsel_prof::MemMode),
    ProfOut(String),
    ProfPmu(String),
    ProfRecordingRuntime(String),
    ProfRuntime(String),
    ProfStackBytes(String),
    ProfTasks,
    ProfileFront,
    PropertyCorpus(String),
    Recipe(String),
    Record(String),
    Recording,
    RecordingRuntime(String),
    Release,
    ReleaseManifest(String),
    Replay,
    ReplayUnit(String),
    Rerun,
    Resident,
    Runtime(String),
    SeedBase(String),
    Sim,
    SimSeed(String),
    SimSeeds(String),
    SourceCore,
    SourceStd,
    Store(String),
    Sysroot(String),
    Tape(String),
    Target(String),
    TestCache(String),
    TestExact(String),
    TestJobs(String),
    TestTimeout(String),
    Transitive,
    Unused,
    WriteLaws,
}
