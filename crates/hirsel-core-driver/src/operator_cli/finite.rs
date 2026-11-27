//! Finite operator reads share one envelope and one exit fold in both renderers.
use super::{class_name, context::Target, operation_id, state_json};
use crate::cli::{self, HelpOption, OpGetOption, Request};
use crate::operator::{Answer, Ask, Class, Phase};
use hirsel_core_ids::hex;
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Display;

#[derive(Clone, Copy)]
pub(super) enum Exit {
    Negative = 1,
    Invalid = 2,
    Blocked = 75,
}

#[derive(Serialize)]
pub(super) struct Failure {
    #[serde(skip)]
    exit: Exit,
    code: &'static str,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    retry: &'static str,
}

impl Failure {
    fn input(field: &'static str, detail: impl Display) -> Self {
        Self {
            exit: Exit::Invalid,
            code: "InvalidInput",
            field: Some(field),
            detail: detail.to_string(),
            retry: "none",
        }
    }
    pub fn context(detail: impl Display) -> Self {
        Self::input("context", detail)
    }
    pub fn damaged(detail: impl Display) -> Self {
        Self {
            exit: Exit::Negative,
            code: "InvalidReply",
            field: None,
            detail: detail.to_string(),
            retry: "none",
        }
    }
    pub fn disconnected() -> Self {
        Self {
            exit: Exit::Blocked,
            code: "Unavailable",
            field: None,
            detail: "host disconnected".into(),
            retry: "read",
        }
    }
    pub fn io(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::PermissionDenied => Self::authority(Class::Denied, error.to_string()),
            ErrorKind::InvalidData => Self::damaged(error),
            _ => Self {
                detail: error.to_string(),
                ..Self::disconnected()
            },
        }
    }
    pub fn authority(class: Class, detail: String) -> Self {
        let exit = match class {
            Class::Conflict | Class::Invalid => Exit::Invalid,
            Class::Busy | Class::Full | Class::Exhausted => Exit::Blocked,
            _ => Exit::Negative,
        };
        Self {
            exit,
            code: class_name(class),
            detail,
            field: None,
            retry: if matches!(exit, Exit::Blocked) {
                "read"
            } else {
                "none"
            },
        }
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    v: u8,
    command: &'a str,
    contract: &'static str,
    exit: i32,
    target: Value,
    operation: Value,
    outcome: Value,
    result: Value,
    error: Option<Failure>,
    next: Vec<Value>,
}

impl<'a> Envelope<'a> {
    fn read(command: &'a str) -> Self {
        Self {
            v: 1,
            command,
            contract: "read",
            exit: 0,
            target: Value::Null,
            operation: Value::Null,
            outcome: json!({"kind":"Observed","effect":"none"}),
            result: Value::Null,
            error: None,
            next: vec![],
        }
    }
    fn fail(&mut self, failure: Failure) {
        self.exit = failure.exit as i32;
        self.outcome = json!({"kind":match failure.exit {
            Exit::Blocked=>"Blocked",Exit::Negative=>"Refused",Exit::Invalid=>"Invalid"
        }, "effect":if self.operation.is_null() {"none"} else {"possible"}});
        self.error = Some(failure);
    }
    fn render(&self, machine: bool) -> i32 {
        if machine {
            println!("{}", serde_json::to_string(self).unwrap());
        } else if let Some(error) = &self.error {
            eprintln!("hirsel: {}: {}", error.code, error.detail);
            if let Some(field) = error.field {
                eprintln!("field {field}");
            }
            eprintln!(
                "{} (exit {})  {}\nretry {}",
                self.command, self.exit, self.outcome, error.retry
            );
            if !self.target.is_null() {
                eprintln!("target {}", self.target);
            }
            if !self.operation.is_null() {
                eprintln!("operation {}", self.operation);
            }
        } else {
            println!("{} (exit {})  {}", self.command, self.exit, self.outcome);
            if !self.target.is_null() {
                println!("target {}\noperation {}", self.target, self.operation);
            }
            println!("{}", serde_json::to_string_pretty(&self.result).unwrap());
            for action in &self.next {
                println!("next {action}");
            }
        }
        self.exit
    }
}

pub(crate) fn invalid(name: &str, detail: String, json: bool) -> i32 {
    let mut envelope = Envelope::read(name);
    envelope.fail(Failure::input("arguments", detail));
    envelope.render(json)
}

pub(crate) fn help(request: Request<HelpOption>) -> i32 {
    let path = request
        .positional
        .into_iter()
        .chain(request.arguments)
        .collect::<Vec<_>>()
        .join(" ");
    let machine = request.options.contains(&HelpOption::Json);
    let target = if path.is_empty() {
        None
    } else {
        match cli::named(&path) {
            Ok(cmd) => Some(cmd),
            Err(e) => return invalid("help", e, machine),
        }
    };
    schema_help(target, machine)
}

pub(crate) fn schema_help(target: Option<&cli::CommandSpec>, machine: bool) -> i32 {
    if !machine {
        print!("{}", cli::help(target));
        return 0;
    }
    let mut envelope = Envelope::read("help");
    envelope.result = match target {
        Some(cmd) => cli::describe(cmd),
        None => json!({"commands":cli::COMMANDS.iter().map(cli::describe).collect::<Vec<_>>()}),
    };
    envelope.render(true)
}

pub(crate) fn get(request: Request<OpGetOption>) -> i32 {
    let machine = request.options.contains(&OpGetOption::Json);
    let mut envelope = Envelope::read("op get");
    if let Err(failure) = observe(request, &mut envelope) {
        envelope.fail(failure);
    }
    envelope.render(machine)
}

fn observe(request: Request<OpGetOption>, envelope: &mut Envelope<'_>) -> Result<(), Failure> {
    let (authority, id) =
        operation_id(request.positional.as_deref()).map_err(|e| Failure::input("operation", e))?;
    let mut context = None;
    for option in &request.options {
        if let OpGetOption::Context(name) = option
            && context.replace(name.as_str()).is_some()
        {
            return Err(Failure::context("--context given twice"));
        }
    }
    let target = Target::resolve(context)?;
    envelope.target = json!(target);
    let qualified = format!("host/{authority:032x}/{}", hex::encode(&id));
    envelope.operation = json!({"kind":"host","authority":format!("{authority:032x}"),"request_id":hex::encode(&id),"id":qualified,"evidence":"authority_catalog"});
    let answer = target.exchange(authority, &Ask::Status(id.to_vec()))?;
    match answer {
        Answer::State(d, group, _, phase) if d == id => {
            target.check_group(&group)?;
            envelope.target["group"] = json!(group.name);
            envelope.target["incarnation"] = json!(group.incarnation.to_string());
            let effect = match &phase {
                Phase::Installed(..) => "committed",
                Phase::Undecided(..) | Phase::Pending(..) => "possible",
                _ => "none",
            };
            envelope.result = state_json(&d, &group, &phase);
            decimal_counters(&mut envelope.result);
            envelope.outcome =
                json!({"kind":"Observed","phase":envelope.result["phase"],"effect":effect});
            if !matches!(phase, Phase::Installed(..) | Phase::Failed(..)) {
                envelope
                    .next
                    .push(json!({"method":"op get","operation":qualified}));
            }
            Ok(())
        }
        Answer::Error(class, detail) => Err(Failure::authority(class, detail)),
        _ => Err(Failure::damaged("host did not return the requested row")),
    }
}

// The legacy Status projection contains only unsigned counters as JSON numbers.
fn decimal_counters(value: &mut Value) {
    match value {
        Value::Number(n) => *value = json!(n.to_string()),
        Value::Array(items) => items.iter_mut().for_each(decimal_counters),
        Value::Object(fields) => fields.values_mut().for_each(decimal_counters),
        _ => {}
    }
}
