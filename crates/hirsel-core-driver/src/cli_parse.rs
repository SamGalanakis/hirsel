use super::help_text::applies;
use super::{Action, CommandSpec, Invocation, OPTIONS, OptionId, help};

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
            let option = OPTIONS
                .iter()
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
        } else if invocation.arguments.is_empty()
            && matches!(
                name,
                "prof annotate" | "prof callers" | "prof callees" | "prof diff" | "prof sql"
            )
        {
            invocation.arguments.push(argument);
        } else {
            return Err(format!("unexpected argument `{argument}` for {name}"));
        }
    }
    Ok(Action::Invoke(invocation))
}
