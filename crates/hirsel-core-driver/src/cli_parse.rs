use super::help_text::{applies, is_help, named};
use super::{CommandSpec, Invocation, OptionId, help, options};

pub enum Action {
    Help(String),
    Version,
    Invoke(Invocation),
}

pub(super) fn invocation(
    command: &'static CommandSpec,
    mut arguments: std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<Action, String> {
    let name = command.name;
    let mut invocation = Invocation {
        command,
        options: Vec::new(),
        positional: None,
        arguments: Vec::new(),
    };
    while let Some(argument) = arguments.next() {
        if name == "prof attach" && (argument == "--mem" || argument.starts_with("--mem=")) {
            return Err(
                "attach cannot see earlier allocations; use 'hirsel prof run --mem'".into(),
            );
        }
        if matches!(name, "prof run" | "prof replay")
            && (argument == "--mem" || argument.starts_with("--mem="))
        {
            let value = if let Some((_, value)) = argument.split_once('=') {
                value.to_owned()
            } else if arguments
                .peek()
                .is_some_and(|v| matches!(v.as_str(), "exact" | "sampled"))
            {
                arguments.next().unwrap()
            } else {
                "sampled".to_owned()
            };
            if !matches!(value.as_str(), "exact" | "sampled") {
                return Err("--mem requires exact or sampled".into());
            }
            invocation.options.push((OptionId::ProfMem, Some(value)));
            continue;
        }
        if argument == "--"
            && matches!(
                name,
                "run"
                    | "prof run"
                    | "call"
                    | "register"
                    | "dry-run"
                    | "dev"
                    | "lean extract"
                    | "lean status"
            )
        {
            invocation.arguments.extend(arguments);
            break;
        }
        if argument.starts_with('-')
            && !(name == "prof sql"
                && argument.starts_with("--")
                && argument.chars().any(char::is_whitespace)
                && invocation.positional.is_some()
                && invocation.arguments.is_empty())
        {
            let option = options()
                .find(|o| o.names.contains(&argument.as_str()) && applies(o, command))
                .ok_or_else(|| {
                    format!("unknown option `{argument}` for {name}; use help {name}")
                })?;
            if option.id == OptionId::Help {
                return Ok(Action::Help(help(Some(command))));
            }
            let value = option
                .value
                .map(|value| {
                    arguments
                        .next()
                        .ok_or_else(|| format!("{argument} requires {value}"))
                })
                .transpose()?;
            invocation.options.push((option.id, value));
        } else if invocation.positional.is_none()
            && !matches!(name, "core-product" | "std-product" | "env")
        {
            invocation.positional = Some(argument);
        } else if name == "coverage merge"
            || invocation.arguments.is_empty()
                && matches!(
                    name,
                    "coverage diff"
                        | "prof annotate"
                        | "prof callers"
                        | "prof callees"
                        | "prof diff"
                        | "prof sql"
                        | "store backup"
                        | "store restore"
                )
        {
            invocation.arguments.push(argument);
        } else {
            return Err(format!("unexpected argument `{argument}` for {name}"));
        }
    }
    Ok(Action::Invoke(invocation))
}

pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut arguments = arguments.into_iter().peekable();
    let Some(mut name) = arguments.next() else {
        return Ok(Action::Help(help(None)));
    };
    if is_help(&name) {
        return Ok(Action::Help(help(None)));
    }
    if name == "--version" {
        return if arguments.next().is_none() {
            Ok(Action::Version)
        } else {
            Err("--version takes no arguments".into())
        };
    }
    if matches!(
        name.as_str(),
        "prof" | "laws" | "lean" | "sdk" | "store" | "coverage"
    ) {
        match arguments.next() {
            None => return Ok(Action::Help(help(Some(named(&name)?)))),
            Some(subcommand) if is_help(&subcommand) => {
                return Ok(Action::Help(help(Some(named(&name)?))));
            }
            Some(subcommand) => name.push_str(&format!(" {subcommand}")),
        }
    }
    let command = named(&name)?;
    if name == "help" {
        let target = arguments
            .next()
            .map(|mut name| {
                if is_help(&name) {
                    Ok(command)
                } else {
                    if matches!(
                        name.as_str(),
                        "prof" | "laws" | "lean" | "sdk" | "store" | "coverage"
                    ) && let Some(subcommand) = arguments.next()
                    {
                        name.push_str(&format!(" {subcommand}"));
                    }
                    named(&name)
                }
            })
            .transpose()?;
        if arguments.next().is_some() {
            return Err("help takes at most one command".into());
        }
        return Ok(Action::Help(help(target)));
    }
    if matches!(name.as_str(), "tape" | "manifest") {
        match arguments.next().as_deref() {
            Some("show") => {}
            Some("--help" | "-h") => return Ok(Action::Help(help(Some(command)))),
            _ => return Err(format!("expected: hirsel {}", command.usage)),
        }
    }
    invocation(command, arguments)
}
