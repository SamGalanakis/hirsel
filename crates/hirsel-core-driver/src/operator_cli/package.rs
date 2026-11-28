use super::finite::{Envelope, Failure};
use crate::cli::{Action, AddOption, DecideOption, Invocation, Request, UpdateOption};
use crate::package_update::plan::{Plan, digest};
use hirsel_config_schema::host_v1 as h;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};

pub(crate) struct EditReceipt<'a> {
    envelope: Envelope<'a>,
    json: bool,
    intent: Option<Intent>,
    decision: Option<h::Decision>,
    action: Option<&'static str>,
    summary: Option<String>,
}

impl<'a> EditReceipt<'a> {
    pub(crate) fn new(command: &'a str, plan: bool, json: bool) -> Self {
        let mut envelope = Envelope::read(command);
        envelope.contract = if plan { "plan" } else { "submit" };
        Self {
            envelope,
            json,
            intent: None,
            decision: None,
            action: None,
            summary: None,
        }
    }

    pub(crate) fn target(
        &mut self,
        workspace: &Path,
        member: Option<String>,
    ) -> Result<(), Failure> {
        let workspace = workspace
            .to_str()
            .ok_or_else(|| Failure::input("workspace", "the path must be UTF-8"))?;
        self.envelope.target = json!({"kind":"workspace", "path":workspace, "member":member});
        self.envelope.operation = json!({"evidence":"local_only"});
        Ok(())
    }

    pub(crate) fn describe(
        &mut self,
        change: &str,
        plan: &Plan,
        evidence: Value,
    ) -> Result<(), Failure> {
        let plan = serde_json::to_value(plan).map_err(|e| Failure::input("plan", e))?;
        let intent = serde_json::to_value(&self.intent).map_err(|e| Failure::input("intent", e))?;
        let hash = plan_digest(self.action.unwrap_or(self.envelope.command), &intent, &plan);
        self.envelope.result = json!({
            "change": change,
            "plan": plan,
            "plan_digest": hash,
            "intent": intent,
            "action": self.action.unwrap_or(self.envelope.command),
            "committed": [],
            "pending": [],
            "evidence": evidence,
        });
        self.envelope.outcome = json!({"kind":"Planned", "effect":"none"});
        Ok(())
    }

    pub(crate) fn authorize(&mut self, expansion: bool) -> Result<bool, Failure> {
        let current = &self.envelope.result["plan_digest"];
        if let Some(decision) = &self.decision {
            if *current != decision.plan_digest {
                return Err(Failure::input(
                    "plan_digest",
                    "the package plan changed; review the current plan",
                ));
            }
            if decision.verdict == h::Verdict::Refuse {
                self.envelope.outcome = json!({"kind":"Declined", "effect":"none"});
                return Ok(false);
            }
        } else if expansion {
            self.review(true);
            return Err(Failure::decision_required(
                "reachable package authority expands",
            ));
        }
        Ok(true)
    }

    pub(crate) fn review(&mut self, expansion: bool) {
        if expansion {
            self.envelope.next.push(json!({
                "method": "op decide",
                "operation": "@RECEIPT.json",
                "plan_digest": self.envelope.result["plan_digest"],
            }));
        }
    }

    pub(crate) fn add_intent(&mut self, cwd: &Path, req: &Request<AddOption>) {
        self.intent = Some(Intent::new(cwd, req, |option, args| match option {
            AddOption::PackagePlan | AddOption::Json => {}
            AddOption::AddAs(value) => pair(args, "--as", value),
            AddOption::AddRoot(value) => pair(args, "--root", value),
            AddOption::PackageStore(value) => pair(args, "--package-store", value),
            AddOption::AddWorkspace => args.push("--workspace".into()),
            AddOption::Locked => args.push("--locked".into()),
            AddOption::Frozen => args.push("--frozen".into()),
        }));
    }

    pub(crate) fn update_intent(&mut self, cwd: &Path, req: &Request<UpdateOption>) {
        self.intent = Some(Intent::new(cwd, req, |option, args| match option {
            UpdateOption::PackagePlan | UpdateOption::Json => {}
            UpdateOption::PackageStore(value) => pair(args, "--package-store", value),
            UpdateOption::Locked => args.push("--locked".into()),
            UpdateOption::Frozen => args.push("--frozen".into()),
        }));
    }

    pub(crate) fn apply(&mut self, committed: &[PathBuf], pending: &[PathBuf]) {
        self.envelope.result["committed"] = json!(committed);
        self.envelope.result["pending"] = json!(pending);
        self.envelope.outcome = json!({
            "kind": if committed.is_empty() { "Unchanged" } else { "Applied" },
            "effect": if committed.is_empty() { "none" } else if pending.is_empty() { "committed" } else { "partial" },
        });
    }

    #[cfg(test)]
    pub(crate) fn has_publication(&self) -> bool {
        self.envelope.outcome["effect"] == "partial"
            || self.envelope.outcome["effect"] == "committed"
    }

    pub(crate) fn summary(&mut self, summary: String) {
        self.summary = Some(summary);
    }

    pub(crate) fn machine(&self) -> bool {
        self.json
    }

    pub(crate) fn finish(mut self, outcome: Result<(), Box<dyn std::error::Error>>) -> i32 {
        if let Err(error) = outcome {
            let failure = match error.downcast::<Failure>() {
                Ok(failure) => *failure,
                Err(error) => Failure::input("package", error),
            };
            self.envelope.fail(failure);
            if self
                .envelope
                .error
                .as_ref()
                .is_some_and(Failure::needs_decision)
            {
                self.envelope.outcome["kind"] = json!("AwaitDecision");
            }
        }
        if !self.json
            && self.envelope.exit == 2
            && self.envelope.result.is_null()
            && let Some(error) = &self.envelope.error
        {
            eprintln!("hirsel: {error}");
            return self.envelope.exit;
        }
        if !self.json
            && self.envelope.exit == 0
            && self.envelope.outcome["kind"] != "Declined"
            && let Some(summary) = &self.summary
        {
            println!("{summary}");
        }
        self.envelope.render(self.json)
    }
}

pub(crate) fn invalid(command: &str, plan: bool, json: bool, detail: String) -> i32 {
    let receipt = EditReceipt::new(command, plan, json);
    receipt.finish(Err(Failure::input("arguments", detail).into()))
}

fn pair(args: &mut Vec<String>, option: &str, value: &str) {
    args.extend([option.to_owned(), value.to_owned()]);
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    directory: PathBuf,
    arguments: Vec<String>,
}

impl Intent {
    fn new<T>(cwd: &Path, req: &Request<T>, option: impl Fn(&T, &mut Vec<String>)) -> Self {
        let mut arguments = vec![req.command.name.to_owned()];
        arguments.extend(req.positional.iter().cloned());
        for value in &req.options {
            option(value, &mut arguments);
        }
        Self {
            directory: cwd.to_path_buf(),
            arguments,
        }
    }
}

fn plan_digest(command: &str, intent: &Value, plan: &Value) -> String {
    digest(
        &serde_json::to_vec(&("hirsel.package-edit-plan.v1", command, intent, plan))
            .expect("a package edit receipt serializes"),
    )
}

#[derive(Deserialize)]
struct Saved {
    v: u8,
    result: SavedPlan,
}

#[derive(Deserialize)]
struct SavedPlan {
    action: String,
    intent: Intent,
    plan: Value,
    plan_digest: String,
}

pub(super) fn decide(req: Request<DecideOption>, path: &str, machine: bool) -> i32 {
    let mut envelope = EditReceipt::new("op decide", false, machine);
    match resume(req, path, &mut envelope) {
        Ok(()) => envelope.finish(Ok(())),
        Err(error) => envelope.finish(Err(error)),
    }
}

fn resume(
    req: Request<DecideOption>,
    path: &str,
    receipt: &mut EditReceipt,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = None;
    for option in req.options {
        match option {
            DecideOption::Json => {}
            DecideOption::DecisionFile(path) => file = Some(path),
            _ => {
                return Err(Failure::input(
                    "arguments",
                    "package decisions accept one --decision and --json",
                )
                .into());
            }
        }
    }
    let saved: Saved = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    let SavedPlan {
        action: command,
        intent,
        plan,
        plan_digest: hash,
    } = saved.result;
    if saved.v != 1
        || !intent.directory.is_absolute()
        || Some(&command) != intent.arguments.first()
        || plan_digest(&command, &serde_json::to_value(&intent)?, &plan) != hash
    {
        return Err(Failure::input("operation", "invalid package edit receipt").into());
    }
    let action = crate::cli::parse(intent.arguments).map_err(|e| Failure::input("operation", e))?;
    if !matches!(
        &action,
        Action::Invoke(Invocation::Add(_) | Invocation::Update(_))
    ) {
        return Err(Failure::input("operation", "expected add or update in the receipt").into());
    }
    let decision = match file.as_deref() {
        Some("-") => super::config::decode(io::stdin().lock())?,
        Some(path) => super::config::decode(File::open(path)?)?,
        None => {
            let mut tty = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|_| {
                    Failure::decision_required("supply --decision FILE or a controlling terminal")
                })?;
            writeln!(tty, "{}", serde_json::to_string_pretty(&plan)?)?;
            let verdict =
                super::decision::read_verdict(&mut BufReader::new(tty.try_clone()?), &mut tty)?;
            h::Decision {
                plan_digest: hash.clone(),
                verdict,
                all: None,
                entries: Vec::new(),
            }
        }
    };
    if decision.plan_digest != hash || decision.all.is_some() || !decision.entries.is_empty() {
        return Err(Failure::input(
            "decision",
            "expected the receipt's plan_digest and no held fates",
        )
        .into());
    }
    receipt.decision = Some(decision);
    // Keep the public command as op decide, but digest the original edit.
    receipt.action = Some(if command == "add" { "add" } else { "update" });
    match action {
        Action::Invoke(Invocation::Add(req)) => {
            crate::package_add::execute(req, &intent.directory, &mut |_| {}, receipt)
        }
        Action::Invoke(Invocation::Update(req)) => {
            crate::package_update::execute(req, &intent.directory, receipt)
        }
        _ => unreachable!("validated package edit"),
    }
}
