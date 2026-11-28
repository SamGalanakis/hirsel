use super::help_text::{children, describe_options, is_help, named};
use super::{
    Argument, CommandSchema, CommandSpec, Invocation, Kind, Multiplicity, ParseError, Positionals,
    Request, Schema,
};
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
    ) -> Result<Action, ParseError> {
        let name = command.name;
        let mut request = Request {
            command,
            options: Vec::new(),
            positional: None,
            arguments: Vec::new(),
        };
        let mut arguments = arguments.peekable();
        let mut once_seen = std::collections::HashSet::new();
        let mut duplicate = None;
        let mut saw_wait = false;
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
                        return Err(ParseError::Arguments(
                            "attach cannot see earlier allocations; use 'hirsel prof run --mem'"
                                .into(),
                        ));
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
                            _ => {
                                return Err(ParseError::Arguments(
                                    "--mem requires exact or sampled".into(),
                                ));
                            }
                        })
                    }
                };
                let canonical = option.flag.names[0];
                saw_wait |= matches!(option.kind, Kind::OptionalWait(_));
                if option.multiplicity == Multiplicity::Once
                    && !once_seen.insert(canonical)
                    && (duplicate.is_none() || name == "op get" && canonical == "--context")
                {
                    duplicate = Some((canonical, saw_wait));
                }
                // Consumers fold last-value options in order, which also validates
                // overridden values and preserves precedence across related flags.
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
                return Err(format!("unexpected argument `{argument}` for {name}").into());
            }
        }
        if matches!(
            self.positionals,
            Positionals::Operation | Positionals::DecisionOperation
        ) {
            let positional = request.positional.as_deref();
            match positional {
                Some(op) if op.starts_with("store/") => {
                    crate::operator_cli::pitr_decision::request_id(op)?;
                }
                Some(op)
                    if matches!(self.positionals, Positionals::DecisionOperation)
                        && op.starts_with('@')
                        && op.len() > 1 => {}
                _ => {
                    crate::operator_cli::operation_id(positional)?;
                }
            }
        }
        if let Some((option, wait_before_duplicate)) = duplicate {
            return Err(ParseError::Duplicate {
                option,
                positional: request.positional,
                wait: if name == "op get" {
                    saw_wait
                } else {
                    wait_before_duplicate
                },
            });
        }
        Ok(Action::Invoke((self.wrap)(request)))
    }

    fn positionals(&self) -> Positionals {
        self.positionals
    }

    fn describe(&self) -> serde_json::Value {
        use serde_json::json;
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
        json!({"options": describe_options(self.options), "operands": operands})
    }

    fn write_help(&self, out: &mut String) {
        for Argument { flag, kind, .. } in self.options {
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
    parse_detailed(arguments).map_err(ParseError::into_detail)
}

pub(crate) fn parse_detailed(
    arguments: impl IntoIterator<Item = String>,
) -> Result<Action, ParseError> {
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
            Err(ParseError::Arguments("--version takes no arguments".into()))
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
