//! Finite operator reads share one envelope and one exit fold in both renderers.
use super::{class_name, context::Target, operation_id, state_json};
use crate::cli::{self, HelpOption, OpGetOption, ParseError, Request};
use crate::operator::{Answer, Ask, Class, Phase};
use hirsel_core_ids::hex;
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Display;

#[derive(Clone, Copy)]
pub(crate) enum Exit {
    Negative = 1,
    Invalid = 2,
    Blocked = 75,
}

#[derive(Serialize)]
pub(crate) struct Failure {
    #[serde(skip)]
    exit: Exit,
    code: &'static str,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    retry: &'static str,
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Failure {}

impl Failure {
    pub(crate) fn input(field: &'static str, detail: impl Display) -> Self {
        Self {
            exit: Exit::Invalid,
            code: "InvalidInput",
            field: Some(field),
            detail: detail.to_string(),
            retry: "none",
        }
    }
    pub(crate) fn without_detail(mut self) -> Self {
        self.detail = String::from("lifecycle observation did not complete");
        self
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
    pub(crate) fn negative(code: &'static str, detail: &str) -> Self {
        Self {
            code,
            detail: detail.into(),
            exit: Exit::Negative,
            field: None,
            retry: "none",
        }
    }

    pub(crate) fn blocked(code: &'static str, detail: &str) -> Self {
        Self {
            code,
            detail: detail.into(),
            ..Self::disconnected()
        }
    }
    pub(crate) fn decision_required(detail: &str) -> Self {
        Self {
            code: "DecisionInputRequired",
            detail: detail.into(),
            exit: Exit::Blocked,
            field: None,
            retry: "none",
        }
    }

    pub(crate) fn needs_decision(&self) -> bool {
        self.code == "DecisionInputRequired"
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
            ErrorKind::Interrupted => Self::blocked("Interrupted", "observer detached"),
            ErrorKind::TimedOut => Self::blocked("Deadline", "observation timed out"),
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
pub(crate) struct Envelope<'a> {
    pub(crate) v: u8,
    pub(crate) command: &'a str,
    pub(crate) contract: &'static str,
    pub(crate) exit: i32,
    pub(crate) target: Value,
    pub(crate) operation: Value,
    pub(crate) outcome: Value,
    pub(crate) result: Value,
    pub(crate) error: Option<Failure>,
    pub(crate) next: Vec<Value>,
}

impl<'a> Envelope<'a> {
    pub(crate) fn read(command: &'a str) -> Self {
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
    pub(crate) fn fail(&mut self, failure: Failure) {
        self.exit = failure.exit as i32;
        self.outcome = json!({"kind":match failure.exit {
            Exit::Blocked=>"Blocked",Exit::Negative=>"Refused",Exit::Invalid=>"Invalid"
        }, "effect":self.outcome["effect"]});
        self.error = Some(failure);
    }
    pub(crate) fn render(&self, machine: bool) -> i32 {
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
            if !self.result.is_null() {
                eprintln!("{}", serde_json::to_string_pretty(&self.result).unwrap());
            }
            if !self.operation.is_null() {
                eprintln!("operation {}", self.operation);
            }
            for action in &self.next {
                eprintln!("next {action}");
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

pub(crate) fn invalid_parse(name: &str, error: ParseError, json: bool) -> i32 {
    let mut env = Envelope::read(name);
    let failure = match error {
        ParseError::Arguments(detail) => Failure::input("arguments", detail),
        ParseError::Duplicate {
            option,
            positional,
            wait,
        } => {
            let store = positional.is_some_and(|s| s.starts_with("store/"));
            env.contract = match name {
                "op get" | "op decide" if wait => "wait",
                "deploy" | "rollback" | "op decide" => "submit",
                "host retire" => "plan",
                _ => "read",
            };
            match name {
                "op get" if store => {
                    Failure::input("options", "a store operation takes --json or --wait")
                }
                "op get" => Failure::input(
                    option.trim_start_matches('-'),
                    format!("{option} given twice"),
                ),
                "op decide" if store => {
                    Failure::input("options", "unexpected or duplicate store decision option")
                }
                "store restore" => Failure::input("arguments", "duplicate or unrelated option"),
                "store archive verify" | "store archive expire" => {
                    Failure::input("options", "duplicate or unrelated archive option")
                }
                _ => Failure::input("arguments", "duplicate option"),
            }
        }
    };
    env.fail(failure);
    env.render(json)
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
    if request
        .options
        .iter()
        .any(|o| matches!(o, OpGetOption::Wait(_)))
    {
        envelope.contract = "wait";
    }
    if let Err(failure) = observe(request, &mut envelope) {
        envelope.fail(failure);
    }
    envelope.render(machine)
}

fn observe(request: Request<OpGetOption>, envelope: &mut Envelope<'_>) -> Result<(), Failure> {
    if request
        .positional
        .as_deref()
        .is_some_and(|s| s.starts_with("store/"))
    {
        return super::pitr_decision::get(request, envelope);
    }
    let (authority, id) =
        operation_id(request.positional.as_deref()).map_err(|e| Failure::input("operation", e))?;
    let mut context = None;
    let mut wait = None;
    for option in request.options {
        match option {
            OpGetOption::Context(name) => context = Some(name),
            OpGetOption::Wait(limit) => wait = Some(super::wait::Wait::start(limit)?),
            OpGetOption::Json => {}
        }
    }
    let target = Target::resolve(context.as_deref())?;
    envelope.target = json!(target);
    let qualified = format!("host/{authority:032x}/{}", hex::encode(&id));
    envelope.operation = json!({"kind":"host","authority":format!("{authority:032x}"),"request_id":hex::encode(&id),"id":qualified,"evidence":"authority_catalog"});
    loop {
        let query = Ask::Status(id.to_vec());
        let answer = target.query(authority, &query, wait.as_ref())?;
        let (d, group, phase) = match answer {
            Answer::State(d, group, _, phase) if d == id => (d, group, phase),
            Answer::Error(class, detail) => return Err(Failure::authority(class, detail)),
            _ => return Err(Failure::damaged("host did not return the requested row")),
        };
        target.check_group(&group)?;
        envelope.target["group"] = json!(group.name);
        envelope.target["incarnation"] = json!(group.incarnation.to_string());
        let effect = match &phase {
            Phase::Retired => "retired",
            Phase::AwaitStore(_) => "awaiting_store",
            Phase::Retiring => "retiring",
            Phase::Installed(..) => "committed",
            Phase::Undecided(..) | Phase::Pending(..) => "possible",
            _ => "none",
        };
        envelope.result = state_json(&d, &group, &phase);
        decimal_counters(&mut envelope.result);
        if matches!(phase, Phase::AwaitDecision(_)) {
            match target.query(authority, &Ask::Plan(id.to_vec()), wait.as_ref())? {
                Answer::Plan(plan, hash) => {
                    super::conversation::validate(&plan, &hash, authority, &id)?;
                    envelope.result["plan"] = super::conversation::plan_json(&plan, &hash);
                }
                Answer::State(..) => continue,
                Answer::Error(class, detail) => return Err(Failure::authority(class, detail)),
                _ => return Err(Failure::damaged("expected the held plan")),
            }
        }
        envelope.outcome =
            json!({"kind":"Observed","phase":envelope.result["phase"],"effect":effect});
        envelope.next.clear();
        if !matches!(
            phase,
            Phase::Retired | Phase::Installed(..) | Phase::Failed(..)
        ) {
            let method = if matches!(phase, Phase::AwaitDecision(..)) {
                "op decide"
            } else {
                "op get"
            };
            envelope
                .next
                .push(json!({"method":method,"operation":qualified}));
        }
        let Some(wait) = &wait else { return Ok(()) };
        let (kind, code) = match phase {
            Phase::Installed(_, _, report)
                if matches!(report.pause, crate::operator::Pause::StartFailed(_)) =>
            {
                ("Failed", 1)
            }
            Phase::Retired | Phase::Installed(..) => ("Completed", 0),
            Phase::Failed(..) => ("Failed", 1),
            Phase::Undecided(..) => ("Undecided", 75),
            Phase::AwaitDecision(..) => ("AwaitDecision", 75),
            _ => {
                wait.poll()?;
                continue;
            }
        };
        envelope.exit = code;
        envelope.outcome["kind"] = json!(kind);
        return Ok(());
    }
}

// The legacy Status projection contains only unsigned counters as JSON numbers.
pub(crate) fn decimal_counters(value: &mut Value) {
    match value {
        Value::Number(n) => *value = json!(n.to_string()),
        Value::Array(items) => items.iter_mut().for_each(decimal_counters),
        Value::Object(fields) => fields.values_mut().for_each(decimal_counters),
        _ => {}
    }
}

pub(crate) fn tape_export(req: Request<cli::TapeOption>) -> i32 {
    use cli::TapeOption as O;
    let json = req.options.contains(&O::Json);
    let mut env = Envelope::read(req.command.name);
    env.contract = "execute";
    let result = (|| {
        let source = req
            .positional
            .ok_or_else(|| Failure::input("tape", "select a tape file"))?;
        let outputs = req
            .options
            .iter()
            .filter_map(|o| match o {
                O::TapeOut(path) => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [out] = outputs.as_slice() else {
            return Err(Failure::input("out", "supply exactly one --out FILE"));
        };
        if !req.options.contains(&O::IncludeCredentials) {
            return Err(Failure::input(
                "include-credentials",
                "tape export requires --include-credentials",
            ));
        }
        crate::tape_cli::export(source.as_ref(), out.as_ref())
            .map_err(|error| Failure::authority(Class::Invalid, error.to_string()))?;
        env.result = json!({"file":out});
        env.outcome = json!({"kind":"Exported","effect":"committed"});
        Ok(())
    })();
    if let Err(error) = result {
        env.fail(error);
    }
    env.render(json)
}
