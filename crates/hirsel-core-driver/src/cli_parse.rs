use super::help_text::{children, is_help, named};
use super::{Argument, CommandSchema, CommandSpec, Invocation, Kind, Positionals, Request, Schema};
use std::fmt::Write;

pub enum Action {
    Help(Option<&'static CommandSpec>),
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
                            || matches!(
                                option.kind,
                                Kind::OptionalWait(_) | Kind::OptionalMem(..) | Kind::RefuseMem
                            ) && argument
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
                            .filter(|value| {
                                !value.starts_with('-')
                                    || value == "-"
                                    || value.parse::<f64>().is_ok()
                            })
                            .ok_or_else(|| format!("{argument} requires {metavar}"))?,
                    ),
                    Kind::OptionalWait(parse) => {
                        let limit = argument
                            .split_once('=')
                            .map(|(_, value)| crate::operator_cli::wait_duration(value))
                            .transpose()?;
                        parse(limit)
                    }
                    Kind::Help => return Ok(Action::Help(Some(command))),
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
            } else if request.positional.is_none()
                && !matches!(self.positionals, Positionals::None | Positionals::Namespace)
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
        if matches!(
            self.positionals,
            Positionals::Operation | Positionals::DecisionOperation
        ) {
            if matches!(self.positionals, Positionals::DecisionOperation)
                && request
                    .positional
                    .as_deref()
                    .is_some_and(|op| op.starts_with('@') && op.len() > 1)
            {
                return Ok(Action::Invoke((self.wrap)(request)));
            }
            if let Some(op) = request
                .positional
                .as_deref()
                .filter(|s| s.starts_with("store/"))
            {
                crate::operator_cli::pitr_decision::request_id(op)?;
            } else {
                crate::operator_cli::operation_id(request.positional.as_deref())?;
            }
        }
        Ok(Action::Invoke((self.wrap)(request)))
    }

    fn positionals(&self) -> Positionals {
        self.positionals
    }

    fn describe(&self) -> serde_json::Value {
        use serde_json::json;
        let flags: Vec<_> = self
            .options
            .iter()
            .filter_map(|a| {
                let (value, syntax) = match a.kind {
                    Kind::OptionalWait(_) => (Some("[=DURATION]"), "optional_equals"),
                    Kind::RefuseMem => return None,
                    Kind::OptionalMem(m, _) => (Some(m), "optional_value"),
                    Kind::Value(m, _) => (Some(m), "value"),
                    _ => (None, "switch"),
                };
                Some(json!({"names":a.flag.names,"value":value,"syntax":syntax,"description":a.flag.description}))
            })
            .collect();
        let operands = match self.positionals {
            Positionals::Namespace => "namespace",
            Positionals::Operation => "host/CATALOG/REQUEST or store/ATTEMPT",
            Positionals::DecisionOperation => {
                "host/CATALOG/REQUEST, store/ATTEMPT or @RECEIPT.json"
            }
            Positionals::None => "none",
            Positionals::One => "one",
            Positionals::Pair => "pair",
            Positionals::Many => "many",
            Positionals::Sql => "sql",
            Positionals::Tail => "guest_tail",
        };
        json!({"options": flags, "operands": operands})
    }

    fn write_help(&self, out: &mut String) {
        for Argument { flag, kind } in self.options {
            let metavar = match kind {
                Kind::OptionalWait(_) => "[=DURATION]".to_owned(),
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
    let Some(name) = arguments.next() else {
        return Ok(Action::Help(None));
    };
    if is_help(&name) {
        return Ok(Action::Help(None));
    }
    if name == "--version" {
        return if arguments.next().is_none() {
            Ok(Action::Version)
        } else {
            Err("--version takes no arguments".into())
        };
    }
    let command = select(named(&name)?, &mut arguments);
    if matches!(command.schema.positionals(), Positionals::Namespace)
        && arguments.peek().is_none_or(|word| is_help(word))
    {
        return Ok(Action::Help(Some(command)));
    }
    command.schema.parse(command, &mut arguments)
}

/// Consume only registered child names. A mixed command such as host or dev
/// can still parse the next word as one of its own operands.
fn select(
    mut command: &'static CommandSpec,
    arguments: &mut std::iter::Peekable<impl Iterator<Item = String>>,
) -> &'static CommandSpec {
    while let Some(word) = arguments.peek() {
        let Some((_, child)) = children(command).find(|(name, _)| word == name) else {
            break;
        };
        arguments.next();
        command = child;
    }
    command
}

pub(crate) fn command_path(args: &[String]) -> &str {
    match args.split_first() {
        Some((name, rest)) => named(name)
            .map(|command| select(command, &mut rest.iter().cloned().peekable()).name)
            .unwrap_or(name),
        None => "help",
    }
}
