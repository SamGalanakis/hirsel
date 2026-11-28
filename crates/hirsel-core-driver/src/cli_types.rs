use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReadContract {
    Retirement,
    Archive,
    PreviewChange,
    PreviewStatus,
    Expiry,
    Restore,
    Help,
    PackageEdit,
    Deploy,
    Decision,
    Operation,
    TapeExport,
    Workloads,
    Workload,
    Lifecycle,
    DevStream,
    DevWait,
    DevStop,
    Rehearsal,
    TapeCheck,
    Execute,
}

#[derive(Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
    pub(crate) contract: Option<ReadContract>,
    pub(super) schema: &'static dyn CommandSchema,
}

#[derive(Debug)]
pub struct Request<T> {
    pub command: &'static CommandSpec,
    pub options: Vec<T>,
    pub positional: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum NoOption {}

#[derive(Debug)]
pub enum Invocation {
    OpGet(Request<OpGetOption>),
    Help(Request<HelpOption>),
    ConfigExport(Request<ConfigExportOption>),
    Compiler(Request<CompilerOption>),
    Simple(Request<NoOption>),
    Lsp(Request<NoOption>),
    RegistryPublish(Request<RegistryPublishOption>),
    Add(Request<AddOption>),
    Update(Request<UpdateOption>),
    Audit(Request<AuditOption>),
    Attach(Request<AttachOption>),
    ProfileQuery(Request<ProfileQueryOption>),
    ProfileExport(Request<ProfileExportOption>),
    Coverage(Request<CoverageOption>),
    Lean(Request<LeanOption>),
    Adopt(Request<AdoptOption>),
    Retire(Request<RetireOption>),
    Host(Request<HostOption>),
    Launcher(Request<LauncherOption>),
    Deploy(Request<DeployOption>),
    Observe(Request<ObserveOption>),
    Decide(Request<DecideOption>),
    Call(Request<CallOption>),
    Dev(Request<DevOption>),
    DevControl(Request<DevControlOption>),
    Store(Request<StoreOption>),
    Serve(Request<ServeOption>),
    Sdk(Request<SdkOption>),
    Tape(Request<TapeOption>),
    New(Request<NewOption>),
    Fmt(Request<FmtOption>),
    Explain(Request<ExplainOption>),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Flag {
    pub names: &'static [&'static str],
    pub description: &'static str,
}

pub(super) struct Switch(pub Flag);
pub(super) struct Value(pub Flag, pub &'static str);

impl Switch {
    pub const fn bind<T>(self, option: T, multiplicity: Multiplicity) -> Argument<T> {
        Argument {
            flag: self.0,
            kind: Kind::Switch(option),
            multiplicity,
        }
    }
}

impl Value {
    pub const fn bind<T>(self, parse: fn(String) -> T, multiplicity: Multiplicity) -> Argument<T> {
        Argument {
            flag: self.0,
            kind: Kind::Value(self.1, parse),
            multiplicity,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Multiplicity {
    Once,
    Repeatable,
    LastWins,
}

#[derive(Debug)]
pub(crate) enum ParseError {
    Arguments(String),
    Duplicate {
        option: &'static str,
        positional: Option<String>,
        wait: bool,
    },
}

impl ParseError {
    pub(crate) fn into_detail(self) -> String {
        match self {
            Self::Arguments(detail) => detail,
            Self::Duplicate { option, .. } => format!("{option} given twice"),
        }
    }
}

impl From<String> for ParseError {
    fn from(detail: String) -> Self {
        Self::Arguments(detail)
    }
}

#[derive(Debug)]
pub(super) struct Argument<T> {
    pub flag: Flag,
    pub kind: Kind<T>,
    pub multiplicity: Multiplicity,
}

#[derive(Debug)]
pub(super) enum Kind<T> {
    Switch(T),
    Value(&'static str, fn(String) -> T),
    Help,
    OptionalWait(fn(Option<std::time::Duration>) -> T),
    OptionalMem(&'static str, fn(hirsel_prof::MemMode) -> T),
    RefuseMem,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Positionals {
    Namespace,
    None,
    One,
    Pair,
    Many,
    Operation,
    DecisionOperation,
    Tail,
    Sql,
}

pub(super) trait CommandSchema: std::fmt::Debug + Sync {
    fn parse(
        &self,
        command: &'static CommandSpec,
        arguments: &mut dyn Iterator<Item = String>,
    ) -> Result<Action, ParseError>;

    fn positionals(&self) -> Positionals;

    fn describe(&self) -> serde_json::Value;

    fn write_help(&self, out: &mut String);
}

#[derive(Debug)]
pub(super) struct Schema<T: 'static> {
    pub positionals: Positionals,
    pub options: &'static [Argument<T>],
    pub wrap: fn(Request<T>) -> Invocation,
}
