use super::{COMMANDS, CommandSpec, OptionId, OptionSpec, options};

pub(crate) fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
}

pub(super) fn is_help(name: &str) -> bool {
    options().any(|option| option.id == OptionId::Help && option.names.contains(&name))
}

pub(super) fn applies(option: &OptionSpec, command: &CommandSpec) -> bool {
    (command.name == "dev"
        && matches!(
            option.id,
            OptionId::Listen
                | OptionId::Limit
                | OptionId::Grant
                | OptionId::Dir
                | OptionId::Backend
                | OptionId::Runtime
                | OptionId::Libraries
                | OptionId::SourceCore
                | OptionId::SourceStd
        ))
        || option.commands.is_empty()
        || option.commands.contains(&command.name)
        || (command.name.starts_with("sdk ")
            && option.commands.contains(&"sdk")
            && (command.name == "sdk ts" || option.id != OptionId::Output))
        || (option.id == OptionId::Json
            && command.name.starts_with("prof ")
            && !matches!(
                command.name,
                "prof run" | "prof replay" | "prof attach" | "prof export"
            ))
        || (command.name == "prof replay"
            && option.commands.contains(&"replay")
            && !matches!(
                option.id,
                OptionId::HostState
                    | OptionId::Diagnose
                    | OptionId::Emulator
                    | OptionId::Sysroot
                    | OptionId::Json
            ))
        || (command.name == "prof run"
            && option.id != OptionId::Output
            && option.commands.contains(&"run"))
}
pub fn help(command: Option<&CommandSpec>) -> String {
    if let Some(command) = command {
        let mut result = format!(
            "Usage: hirsel {}\n\n{}\n\nOptions:\n",
            command.usage, command.description
        );
        if matches!(command.name, "prof" | "lean" | "sdk") {
            let prefix = format!("{} ", command.name);
            for subcommand in COMMANDS.iter().filter(|c| c.name.starts_with(&prefix)) {
                result.push_str(&format!(
                    "  {}\n    {}\n",
                    subcommand.usage, subcommand.description
                ));
            }
            return result;
        }
        for option in options().filter(|o| applies(o, command)) {
            let label = format!(
                "{}{}",
                option.names.join(", "),
                option.value.map_or(String::new(), |v| format!(" {v}"))
            );
            result.push_str(&format!("  {label:<34} {}\n", option.description));
        }
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
