use super::*;

#[derive(Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
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
    Compiler(Request<CompilerOption>),
    Simple(Request<NoOption>),
    Add(Request<AddOption>),
    Attach(Request<AttachOption>),
    ProfileQuery(Request<ProfileQueryOption>),
    ProfileExport(Request<ProfileExportOption>),
    Coverage(Request<CoverageOption>),
    Lean(Request<LeanOption>),
    Host(Request<HostOption>),
    Launcher(Request<LauncherOption>),
    Register(Request<RegisterOption>),
    Deploy(Request<DeployOption>),
    Decide(Request<DecideOption>),
    Call(Request<CallOption>),
    Dev(Request<DevOption>),
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
    pub const fn bind<T>(self, option: T) -> Argument<T> {
        Argument {
            flag: self.0,
            kind: Kind::Switch(option),
        }
    }
}

impl Value {
    pub const fn bind<T>(self, parse: fn(String) -> T) -> Argument<T> {
        Argument {
            flag: self.0,
            kind: Kind::Value(self.1, parse),
        }
    }
}

#[derive(Debug)]
pub(super) struct Argument<T> {
    pub flag: Flag,
    pub kind: Kind<T>,
}

#[derive(Debug)]
pub(super) enum Kind<T> {
    Switch(T),
    Value(&'static str, fn(String) -> T),
    Help,
    OptionalMem(&'static str, fn(hirsel_prof::MemMode) -> T),
    RefuseMem,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Positionals {
    None,
    One,
    Pair,
    Many,
    Tail,
    Sql,
}

pub(super) trait CommandSchema: std::fmt::Debug + Sync {
    fn parse(
        &self,
        command: &'static CommandSpec,
        arguments: &mut dyn Iterator<Item = String>,
    ) -> Result<Action, String>;

    fn write_help(&self, out: &mut String);
}

#[derive(Debug)]
pub(super) struct Schema<T: 'static> {
    pub positionals: Positionals,
    pub options: &'static [Argument<T>],
    pub wrap: fn(Request<T>) -> Invocation,
}
