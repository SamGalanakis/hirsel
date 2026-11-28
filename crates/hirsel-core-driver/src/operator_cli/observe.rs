//! Read a pinned catalog without requesting a start or making a store observation.
use super::{
    context::Target,
    finite::{Envelope, Failure},
    wait::Wait,
};
use crate::{
    cli::{ObserveOption as O, Request},
    launcher::session::Group,
    operator::{Answer, Ask, EventPosition, Health, Lifecycle, OperationPhase, Workload},
};
use hirsel_core_ids::{ContentHash, hex};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn selection<'a>(
    target: &'a Target,
    written: Option<&'a str>,
) -> Result<Option<(&'a str, Option<u64>)>, Failure> {
    let Some(written) = written.or_else(|| target.name()) else {
        return Ok(None);
    };
    let (name, incarnation) = match written.split_once('@') {
        Some((name, id)) => (
            name,
            Some(
                id.parse()
                    .map_err(|_| Failure::input("group", "expected NAME[@INCARNATION]"))?,
            ),
        ),
        None => (written, None),
    };
    if name.is_empty() || target.name().is_some_and(|fixed| fixed != name) {
        return Err(Failure::input(
            "group",
            "group contradicts the selected context",
        ));
    }
    let pinned = target.incarnation()?;
    if pinned.zip(incarnation).is_some_and(|(a, b)| a != b) {
        return Err(Failure::input(
            "group",
            "incarnation contradicts the selected context",
        ));
    }
    Ok(Some((name, incarnation.or(pinned))))
}

fn answer(
    target: &Target,
    authority: u128,
    request: &Ask,
    wait: Option<&Wait>,
) -> Result<Answer, Failure> {
    match target.query(authority, request, wait)? {
        Answer::Error(class, detail) => Err(Failure::authority(class, detail)),
        answer => Ok(answer),
    }
}

fn health_name(health: Health) -> &'static str {
    match health {
        Health::Serving => "Serving",
        Health::Starting => "Installed",
        Health::Cold => "Cold",
        Health::Failed => "StartFailed",
        Health::Unknown => "Unknown",
    }
}

fn phase_name(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Input => "AwaitInput",
        OperationPhase::Admission => "Admitting",
        OperationPhase::Confirm => "AwaitConfirm",
        OperationPhase::Assembly => "Assembling",
        OperationPhase::Prepare => "Preparing",
        OperationPhase::Pending => "Pending",
        OperationPhase::Undecided => "Undecided",
        OperationPhase::Decision => "AwaitDecision",
        OperationPhase::Installed => "Installed",
        OperationPhase::Failed => "Failed",
    }
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().try_into().unwrap_or(u64::MAX))
}

const FRESHNESS_MS: u64 = 5_000;

fn age(row: &Workload) -> u64 {
    wall_ms().saturating_sub(row.observed_at)
}

fn row_json(row: &Workload, authority: u128) -> Value {
    json!({
        "group": row.group.name, "incarnation": row.group.incarnation.to_string(),
        "source":"authority_catalog", "observed_at":row.observed_at.to_string(),
        "age_ms":age(row).to_string(), "freshness_ms":FRESHNESS_MS.to_string(),
        "stale":age(row) > FRESHNESS_MS, "health":health_name(row.health),
        "activation":row.activation.to_string(), "assembly":hex::encode(&row.assembly),
        "generation":row.generation.map(|n| n.to_string()),
        "store_id":row.store_id.as_ref().map(|id| format!("{:016x}{:016x}", id.hi, id.lo)),
        "operation":row.operation.as_ref().map(|o| json!({
            "id":format!("host/{authority:032x}/{}", hex::encode(&o.request)),
            "phase":phase_name(o.phase)
        }))
    })
}

pub(crate) fn run(req: Request<O>) -> i32 {
    let json = req.options.contains(&O::Json);
    let mut env = Envelope::read(req.command.name);
    if req.command.name == "logs" {
        let mut output = io::stdout().lock();
        let mut stream = Stream {
            output: &mut output,
            subject: Value::Null,
            cursor: None,
            observed_at: None,
        };
        if let Err(failure) = observe(req, &mut env, Some(&mut stream)) {
            env.fail(failure.without_detail());
        }
        let _ = stream.write("end", Value::Null, serde_json::to_value(&env).unwrap());
        return env.exit;
    }
    if let Err(failure) = observe(req, &mut env, None) {
        env.fail(failure);
    }
    env.render(json)
}

pub(crate) fn invalid_logs() -> i32 {
    let mut env = Envelope::read("logs");
    env.fail(Failure::input(
        "arguments",
        "invalid lifecycle command arguments",
    ));
    let mut stdout = io::stdout().lock();
    let mut stream = Stream {
        output: &mut stdout,
        subject: Value::Null,
        cursor: None,
        observed_at: None,
    };
    let _ = stream.write("end", Value::Null, serde_json::to_value(&env).unwrap());
    env.exit
}

fn observe(
    req: Request<O>,
    env: &mut Envelope<'_>,
    stream: Option<&mut Stream<'_>>,
) -> Result<(), Failure> {
    let (mut context, mut group, mut after) = (None, None, None);
    let (mut ready, mut follow) = (false, false);
    let mut seen = HashSet::new();
    for option in req.options {
        if !seen.insert(std::mem::discriminant(&option)) {
            return Err(Failure::input("arguments", "duplicate option"));
        }
        match option {
            O::Context(value) => context = Some(value),
            O::GroupName(value) => group = Some(value),
            O::After(value) => after = Some(value),
            O::Ready => ready = true,
            O::Follow => follow = true,
            O::Json => (),
        }
    }
    if follow || ready {
        env.contract = "wait";
    }
    if context.as_deref() == Some("dev") {
        if env.command != "status" || group.is_some() {
            return Err(Failure::input(
                "context",
                "use status [DIR] --context dev without --group",
            ));
        }
        return dev_status(req.positional, ready, env);
    }
    if req.positional.is_some() {
        return Err(Failure::input(
            "directory",
            "a directory requires --context dev",
        ));
    }
    let target = Target::resolve(context.as_deref())?;
    let authority = target.authority()?;
    env.target = serde_json::to_value(&target).unwrap();
    let group = selection(&target, group.as_deref())?;
    if env.command == "ps" && group.is_none() {
        return match answer(&target, authority, &Ask::Workloads(after), None)? {
            Answer::Workloads(rows, next) => {
                env.result = json!({"groups":rows.iter().map(|row| row_json(row, authority)).collect::<Vec<_>>(),"after":next});
                Ok(())
            }
            _ => Err(Failure::damaged("expected workload page")),
        };
    }
    let (name, incarnation) = group.ok_or_else(|| Failure::input("group", "select a group"))?;
    if env.command == "ps" && after.is_some() {
        return Err(Failure::input(
            "after",
            "a group selection has a single page",
        ));
    }
    let Answer::Workload(row) = answer(
        &target,
        authority,
        &Ask::Workload(name.into(), incarnation),
        None,
    )?
    else {
        return Err(Failure::damaged("expected workload observation"));
    };
    target.check_group(&row.group)?;
    if row.group.name != name || incarnation.is_some_and(|i| i != row.group.incarnation) {
        return Err(Failure::damaged("the observation names another group"));
    }
    env.target["group"] = json!(row.group.name);
    env.target["incarnation"] = json!(row.group.incarnation.to_string());
    let mut rendered = row_json(&row, authority);
    if env.command == "ps" {
        env.result = json!({"groups":[rendered],"after":null});
        return Ok(());
    }
    if let Some(stream) = stream {
        return logs(
            &target,
            authority,
            row.group,
            after.as_deref(),
            follow,
            env,
            stream,
        );
    }
    rendered["archive"] = json!({"state":"outside_coverage","observed_at":null,"age_ms":null,"archived_through":null,"acked_unsettled":null,"settled_through":null});
    rendered["recipe"] = recipe(&target, &row);
    env.result = rendered;
    if ready {
        match row.health {
            Health::Serving if age(&row) <= FRESHNESS_MS => (),
            Health::Cold | Health::Failed if age(&row) <= FRESHNESS_MS => {
                return Err(Failure::negative(
                    health_name(row.health),
                    "serving evidence is negative",
                ));
            }
            _ => {
                return Err(Failure::blocked(
                    health_name(row.health),
                    "fresh serving evidence is unavailable",
                ));
            }
        }
    }
    Ok(())
}

fn recipe(target: &Target, row: &Workload) -> Value {
    let value = (|| {
        let state = target.state().ok()?;
        let assembly = ContentHash(row.assembly.as_slice().try_into().ok()?);
        let records = crate::launcher::objects::Records::read_only(&state.join("objects")).ok()?;
        Some(records.assembly(assembly).ok()?.recipe.to_string())
    })();
    json!(value)
}

fn dev_status(
    directory: Option<String>,
    ready: bool,
    env: &mut Envelope<'_>,
) -> Result<(), Failure> {
    crate::request::launch::dev::control::status(directory.as_deref(), ready, env)
}

struct Stream<'a> {
    output: &'a mut dyn Write,
    subject: Value,
    cursor: Option<String>,
    observed_at: Option<u64>,
}

impl Stream<'_> {
    fn write(&mut self, kind: &str, operation: Value, payload: Value) -> Result<(), Failure> {
        let record = json!({
            "v":1,"kind":kind,"subject":self.subject,"operation":operation,"cursor":self.cursor,
            "observed_at":self.observed_at.map(|v| v.to_string()),"payload":payload
        });
        serde_json::to_writer(&mut self.output, &record)
            .map_err(|_| Failure::blocked("Interrupted", "output closed"))?;
        self.output
            .write_all(b"\n")
            .and_then(|()| self.output.flush())
            .map_err(|_| Failure::blocked("Interrupted", "output closed"))
    }
}

fn cursor(authority: u128, incarnation: u64, stream: u64, sequence: u64) -> String {
    format!("{authority:032x}{incarnation:016x}{stream:016x}{sequence:016x}")
}

fn position(raw: &str, authority: u128, incarnation: u64) -> Result<EventPosition, Failure> {
    let bad = || Failure::input("after", "cursor is invalid for this subject");
    if raw.len() != 80 {
        return Err(bad());
    }
    let bytes: [u8; 40] = hex::decode(raw)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(bad)?;
    if u128::from_be_bytes(bytes[..16].try_into().unwrap()) != authority
        || u64::from_be_bytes(bytes[16..24].try_into().unwrap()) != incarnation
    {
        return Err(bad());
    }
    Ok(EventPosition {
        stream: u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
        sequence: u64::from_be_bytes(bytes[32..].try_into().unwrap()),
    })
}

fn logs(
    target: &Target,
    authority: u128,
    group: Group,
    after: Option<&str>,
    follow: bool,
    env: &mut Envelope<'_>,
    stream: &mut Stream<'_>,
) -> Result<(), Failure> {
    stream.subject = json!({"authority":format!("{authority:032x}"),"group":group.name,"incarnation":group.incarnation.to_string()});
    let mut after = after
        .map(|value| position(value, authority, group.incarnation))
        .transpose()?;
    let wait = follow.then(|| Wait::start(None)).transpose()?;
    loop {
        let reading = Ask::Events(
            Group {
                name: group.name.clone(),
                incarnation: group.incarnation,
            },
            after.clone(),
        );
        let Answer::Events(page) = answer(target, authority, &reading, wait.as_ref())? else {
            return Err(Failure::damaged("expected lifecycle event page"));
        };
        let at = |seq| cursor(authority, group.incarnation, page.stream, seq);
        stream.observed_at = Some(page.observed_at);
        if page.gap {
            let lost = after.ok_or_else(|| Failure::damaged("unsolicited lifecycle gap"))?;
            stream.cursor = Some(cursor(
                authority,
                group.incarnation,
                lost.stream,
                lost.sequence,
            ));
            stream.write(
                "gap",
                Value::Null,
                json!({"last_known":stream.cursor,"oldest_available":at(page.oldest)}),
            )?;
            return Err(Failure::blocked(
                "CursorGap",
                "refresh the lifecycle snapshot",
            ));
        }
        if after.is_none() {
            stream.cursor = Some(at(page
                .events
                .first()
                .map_or(page.through, |event| event.sequence - 1)));
            stream.write("snapshot", Value::Null, json!({"oldest":at(page.oldest),"through":at(page.through),"latest":at(page.latest),"pruned":page.oldest > 1}))?;
        }
        for event in page.events {
            if event.incarnation != group.incarnation {
                return Err(Failure::damaged("event names a different group"));
            }
            stream.cursor = Some(at(event.sequence));
            stream.observed_at = Some(event.observed_at);
            let kind = match event.kind {
                Lifecycle::Ready => "Ready",
                Lifecycle::Stopped => "Stopped",
                Lifecycle::StartFailed => "StartFailed",
                Lifecycle::Installed => "Installed",
                Lifecycle::Operation(phase) => phase_name(phase),
            };
            let operation = event.request.map_or(Value::Null, |d| {
                json!({
                    "authority":format!("{authority:032x}"),"request_id":hex::encode(&[d.first.to_le_bytes(), d.second.to_le_bytes()].concat())
                })
            });
            stream.write(
                "event",
                operation,
                json!({
                    "kind":kind,"activation":event.activation.map(|n| n.to_string()),
                    "generation":event.generation.map(|n| n.to_string())
                }),
            )?;
        }
        stream.cursor = Some(at(page.through));
        stream.observed_at = Some(page.observed_at);
        env.result = json!({"cursor":stream.cursor, "truncated":page.through < page.latest});
        let Some(wait) = &wait else { return Ok(()) };
        after = Some(EventPosition {
            stream: page.stream,
            sequence: page.through,
        });
        if page.through == page.latest {
            wait.poll()?;
        }
    }
}
