use super::help_text::{is_help, named};
use super::{
    Argument, CommandSchema, CommandSpec, Invocation, Kind, Positionals, Request, Schema, help,
};
use std::fmt::Write;

pub enum Action {
    Help(String),
    Version,
    Invoke(Invocation),
}

impl<T: Clone + std::fmt::Debug + Sync> CommandSchema for Schema<T> {
    fn parse(
        &self,
        command: &'static CommandSpec,
        arguments: &mut dyn Iterator<Item = String>,
    ) -> Result<Action, String> {
        let name = command.name;
        let mut request = Request {
            command,
            options: Vec::new(),
            positional: None,
            arguments: Vec::new(),
        };
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            if argument == "--" && matches!(self.positionals, Positionals::Tail) {
                request.arguments.extend(arguments);
                break;
            }
            let sql_comment = matches!(self.positionals, Positionals::Sql)
                && argument.starts_with("--")
                && argument.chars().any(char::is_whitespace)
                && request.positional.is_some()
                && request.arguments.is_empty();
            if argument.starts_with('-') && !sql_comment {
                let option = self
                    .options
                    .iter()
                    .find(|option| {
                        option.flag.names.contains(&argument.as_str())
                            || matches!(option.kind, Kind::OptionalMem(..) | Kind::RefuseMem)
                                && argument
                                    .split_once('=')
                                    .is_some_and(|(name, _)| option.flag.names.contains(&name))
                    })
                    .ok_or_else(|| {
                        format!("unknown option `{argument}` for {name}; use help {name}")
                    })?;
                let value = match &option.kind {
                    Kind::Switch(value) => value.clone(),
                    Kind::Value(metavar, parse) => parse(
                        arguments
                            .next()
                            .ok_or_else(|| format!("{argument} requires {metavar}"))?,
                    ),
                    Kind::Help => return Ok(Action::Help(help(Some(command)))),
                    Kind::RefuseMem => {
                        return Err(
                            "attach cannot see earlier allocations; use 'hirsel prof run --mem'"
                                .into(),
                        );
                    }
                    Kind::OptionalMem(_, parse) => {
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
                        parse(match value.as_str() {
                            "exact" => hirsel_prof::MemMode::Exact,
                            "sampled" => hirsel_prof::MemMode::Sampled,
                            _ => return Err("--mem requires exact or sampled".into()),
                        })
                    }
                };
                request.options.push(value);
            } else if request.positional.is_none() && !matches!(self.positionals, Positionals::None)
            {
                request.positional = Some(argument);
            } else if matches!(self.positionals, Positionals::Many)
                || request.arguments.is_empty()
                    && matches!(self.positionals, Positionals::Pair | Positionals::Sql)
            {
                request.arguments.push(argument);
            } else {
                return Err(format!("unexpected argument `{argument}` for {name}"));
            }
        }
        Ok(Action::Invoke((self.wrap)(request)))
    }

    fn write_help(&self, out: &mut String) {
        for Argument { flag, kind } in self.options {
            let metavar = match kind {
                Kind::RefuseMem => continue,
                Kind::Value(metavar, _) | Kind::OptionalMem(metavar, _) => format!(" {metavar}"),
                Kind::Switch(_) | Kind::Help => String::new(),
            };
            let label = format!("{}{metavar}", flag.names.join(", "));
            writeln!(out, "  {label:<34} {}", flag.description).unwrap();
        }
    }
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
        "prof" | "laws" | "lean" | "sdk" | "store" | "coverage" | "tapes" | "fleet"
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
                        "prof" | "laws" | "lean" | "sdk" | "store" | "coverage" | "tapes" | "fleet"
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
    command.schema.parse(command, &mut arguments)
}
