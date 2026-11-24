use super::OptionId;

#[derive(Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
}

#[derive(Debug)]
pub struct Invocation {
    pub command: &'static CommandSpec,
    pub options: Vec<(OptionId, Option<String>)>,
    pub positional: Option<String>,
    pub arguments: Vec<String>,
}
