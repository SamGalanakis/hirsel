//! Typed arguments shared by commands with the same interpreter.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelpOption {
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpGetOption {
    Wait(Option<std::time::Duration>),
    Context(String),
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryPublishOption {
    PublishWorkspace,
    PublishKey(String),
    PublishWorkload(String),
    PublishTokenCommand(String),
    PublishGeneration(String),
    PublishEnroll(String),
    PublishDescription(String),
    PublishSourceCommit(String),
    PackageStore(String),
    Libraries(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddOption {
    PackagePlan,
    AddRoot(String),
    AddAs(String),
    AddWorkspace,
    Frozen,
    Json,
    Locked,
    PackageStore(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOption {
    PackagePlan,
    Frozen,
    Json,
    Locked,
    PackageStore(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOption {
    Frozen,
    Json,
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
pub enum DeployOption {
    Recipe(String),
    SourceDir(String),
    CodeStore(String),
    Create(String),
    Plan,
    Wait(Option<std::time::Duration>),
    RequestId(String),
    Context(String),
    GroupName(String),
    Json,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObserveOption {
    Context(String),
    GroupName(String),
    After(String),
    Ready,
    Follow,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdoptOption {
    AdoptSpec(String),
    AdoptAssembly(String),
    Context(String),
    RequestId(String),
    Plan,
    Wait(Option<std::time::Duration>),
    Json,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetireOption {
    GroupName(String),
    Context(String),
    RequestId(String),
    Plan,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecideOption {
    DecisionFile(String),
    Wait(Option<std::time::Duration>),
    Context(String),
    Json,
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
    Json,
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

#[derive(Clone, Debug, PartialEq)]
pub enum DevControlOption {
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOption {
    Deep,
    KeepFrom(String),
    ArchiveAt(String),
    RestoreFrom(String),
    RestoreTarget(String),
    RestoreTo(String),
    Plan,
    Json,
    GroupName(String),
    HostState(String),
    RecordedExecutable(String),
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
    Emit(String),
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
    TapeOut(String),
    IncludeCredentials,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewOption {
    Lib,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FmtOption {
    FmtCheck,
    FmtStdin,
    FmtData,
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
    EmbedRuntime(String),
    ExecHelper(String),
    Emit(String),
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
    ReplayOutputs(String),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigExportOption {
    Json,
}
