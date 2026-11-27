use super::{COMMANDS, CommandSpec, HELP};

pub(crate) fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
}

pub(super) fn is_help(name: &str) -> bool {
    HELP.names.contains(&name)
}

pub fn help(command: Option<&CommandSpec>) -> String {
    if let Some(command) = command {
        let mut result = format!(
            "Usage: hirsel {}\n\n{}\n\nOptions:\n",
            command.usage, command.description
        );
        if matches!(command.name, "op" | "prof" | "lean" | "sdk" | "fleet") {
            let prefix = format!("{} ", command.name);
            for subcommand in COMMANDS.iter().filter(|c| c.name.starts_with(&prefix)) {
                result.push_str(&format!(
                    "  {}\n    {}\n",
                    subcommand.usage, subcommand.description
                ));
            }
            return result;
        }
        command.schema.write_help(&mut result);
        result
    } else {
        let mut result =
            "Usage: hirsel <command> [options]\n       hirsel --version\n\nCommands:\n".to_owned();
        for command in COMMANDS.iter().filter(|c| !c.name.contains(' ')) {
            result.push_str(&format!("  {:<16} {}\n", command.name, command.description));
        }
        result.push_str("\nUse help <command>, <command> --help, or <command> -h for options.\n");
        result
    }
}

pub(crate) fn describe(command: &CommandSpec) -> serde_json::Value {
    use super::types::ReadContract;
    use serde_json::json;
    let mut spec = command.schema.describe();
    spec["name"] = json!(command.name);
    spec["usage"] = json!(command.usage);
    spec["description"] = json!(command.description);
    if let Some(contract) = command.contract {
        let (target, authority, predicate, receipt) = match contract {
            ReadContract::Help => ("none", "local", "command schema read", false),
            ReadContract::Operation => ("host", "operator_uid", "catalog row read", true),
        };
        spec["result"] = json!({"type":"finite", "v":1, "contract":"read",
            "target_kind":target, "authority":authority, "completion":predicate,
            "receipt":receipt, "credential_bearing":false, "mutually_exclusive":[]});
    }
    spec
}
