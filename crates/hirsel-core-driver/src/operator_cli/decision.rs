//! The controlling terminal and data file both produce a host_v1.Decision.
use super::{
    config,
    context::Target,
    conversation::{Exchange, current, locate, plan_json},
    finite::{Envelope, Failure},
    receipt::{Location, digest},
    wait::Wait,
};
use crate::{
    cli::{DecideOption as O, Request},
    operator::{Answer, Ask, DecisionStage, DeployPlan, Fates, HeldFate, Input, Verdict},
};
use hirsel_config_schema::host_v1 as h;
use hirsel_core_ids::hex;
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
};

fn paths(plan: &DeployPlan) -> BTreeSet<&[u8]> {
    plan.facts
        .triggers
        .rows
        .iter()
        .filter(|r| r.class == 1)
        .map(|r| r.path.as_slice())
        .collect()
}

fn choice(text: &str) -> Result<h::Fate, Failure> {
    match text {
        "release" => Ok(h::Fate::Release),
        "cancel" => Ok(h::Fate::Cancel),
        _ => Err(Failure::input("fates", "expected release or cancel")),
    }
}
fn line(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    prompt: &str,
) -> Result<String, Failure> {
    write!(writer, "{prompt} ")
        .and_then(|()| writer.flush())
        .map_err(Failure::io)?;
    let mut s = String::new();
    reader.read_line(&mut s).map_err(Failure::io)?;
    Ok(s.trim().to_owned())
}
pub(super) fn read_verdict(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<h::Verdict, Failure> {
    Ok(
        match line(reader, writer, "Verdict (approve/refuse)?")?.as_str() {
            "approve" => h::Verdict::Approve,
            "refuse" => h::Verdict::Refuse,
            _ => {
                return Err(Failure::input(
                    "verdict",
                    "an explicit approve or refuse is required",
                ));
            }
        },
    )
}
pub(super) fn interactive(
    p: &DeployPlan,
    h: &[u8],
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<h::Decision, Failure> {
    writeln!(
        writer,
        "{}",
        serde_json::to_string_pretty(&plan_json(p, h)).unwrap()
    )
    .map_err(Failure::io)?;
    let verdict = read_verdict(reader, writer)?;
    let paths = paths(p);
    let mut all = None;
    let mut entries = Vec::new();
    if verdict == h::Verdict::Approve && !paths.is_empty() {
        let coverage = line(reader, writer, "Fates (each, release all, cancel all)?")?;
        match coverage.as_str() {
            "release all" => all = Some(h::Fate::Release),
            "cancel all" => all = Some(h::Fate::Cancel),
            "each" => {
                for path in paths {
                    let name = std::str::from_utf8(path).map_err(Failure::damaged)?;
                    let fate = choice(&line(
                        reader,
                        writer,
                        &format!("{name:?} (release/cancel)?"),
                    )?)?;
                    entries.push((name.to_owned(), fate));
                }
            }
            _ => {
                return Err(Failure::input(
                    "fates",
                    "expected each, release all or cancel all",
                ));
            }
        }
    }
    Ok(h::Decision {
        plan_digest: hex::encode(h),
        verdict,
        all,
        entries,
    })
}
pub(super) fn frame(
    id: &[u8],
    plan: &DeployPlan,
    hash: &[u8],
    doc: h::Decision,
) -> Result<Ask, Failure> {
    if hex::decode(&doc.plan_digest).as_deref() != Some(hash) {
        return Err(Failure::input("plan_digest", "the held plan changed"));
    }
    let convert = |v| match v {
        h::Fate::Release => HeldFate::Release,
        h::Fate::Cancel => HeldFate::Cancel,
    };
    let mut fates = Fates {
        all: doc.all.map(convert),
        entries: doc
            .entries
            .into_iter()
            .map(|(p, f)| (p.into_bytes(), convert(f)))
            .collect(),
    };
    fates.entries.sort_by(|a, b| a.0.cmp(&b.0));
    let held = paths(plan);
    let valid = match doc.verdict {
        h::Verdict::Refuse => fates.entries.is_empty() && fates.all.is_none(),
        h::Verdict::Approve if fates.all.is_some() => !held.is_empty() && fates.entries.is_empty(),
        h::Verdict::Approve => held
            .into_iter()
            .eq(fates.entries.iter().map(|(p, _)| p.as_slice())),
    };
    if !valid {
        return Err(Failure::input(
            "fates",
            "choices must cover exactly the held paths",
        ));
    }
    Ok(Ask::Decide(
        id.into(),
        hash.into(),
        match doc.verdict {
            h::Verdict::Refuse => Verdict::Refuse,
            h::Verdict::Approve => Verdict::Approve(fates),
        },
    ))
}
pub(crate) fn decide(req: Request<O>) -> i32 {
    let json = req.options.contains(&O::Json);
    if let Some(path) = req
        .positional
        .as_deref()
        .and_then(|op| op.strip_prefix('@'))
        .map(str::to_owned)
    {
        return super::package::decide(req, &path, json);
    }
    let mut env = Envelope::read("op decide");
    env.contract = "submit";
    if let Err(f) = apply(req, &mut env) {
        env.fail(f);
    }
    env.render(json)
}
fn apply(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    if req
        .positional
        .as_deref()
        .is_some_and(|s| s.starts_with("store/"))
    {
        return super::pitr_decision::decide(req, env);
    }
    let (authority, id) = super::operation_id(req.positional.as_deref())
        .map_err(|e| Failure::input("operation", e))?;
    let (mut context, mut file, mut wait) = (None, None, None);
    for o in req.options {
        match o {
            O::Context(v) if context.is_none() => context = Some(v),
            O::DecisionFile(v) if file.is_none() => file = Some(v),
            O::Wait(w) if wait.is_none() => {
                wait = Some(Wait::start(w)?);
                env.contract = "wait";
            }
            O::Json => (),
            _ => return Err(Failure::input("arguments", "duplicate option")),
        }
    }
    let target = Target::resolve(context.as_deref())?;
    let loc = Location::new(authority, &id)?;
    let input = match target.query(authority, &Ask::Status(id.into()), wait.as_ref())? {
        Answer::State(d, g, input, _) if d == id => {
            target.check_group(&g)?;
            input
        }
        Answer::Error(c, v) => return Err(Failure::authority(c, v)),
        _ => return Err(Failure::damaged("expected the requested row")),
    };
    locate(env, &target, &loc, authority, &id);
    let (plan, hash) = current(&target, authority, &id, wait.as_ref())?;
    env.result = plan_json(&plan, &hash);
    let doc = match file.as_deref() {
        Some("-") => config::decode(io::stdin().lock())?,
        Some(p) => config::decode(File::open(p).map_err(Failure::io)?)?,
        None => {
            let mut tty = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|_| {
                    Failure::blocked(
                        "DecisionInputRequired",
                        "supply --decision FILE or a controlling terminal",
                    )
                })?;
            let mut reader = BufReader::new(tty.try_clone().map_err(Failure::io)?);
            interactive(&plan, &hash, &mut reader, &mut tty)?
        }
    };
    let ask = frame(&id, &plan, &hash, doc)?;
    let mut prepared = None;
    if matches!(ask, Ask::Decide(_, _, Verdict::Approve(_)))
        && matches!(input, Input::Attested(_) | Input::Source(_))
    {
        if let Some(receipt) = loc.read()? {
            let p = receipt.inputs.open(target.state()?)?;
            if digest(&p.input) != hex::encode(&plan.input_digest)
                || receipt.intent_digest != hex::encode(&plan.intent_digest)
            {
                return Err(Failure::input(
                    "input",
                    "local input differs from the held request",
                ));
            }
            prepared = Some(p);
        } else if matches!(input, Input::Attested(_)) || plan.facts.stage == DecisionStage::Creation
        {
            return Err(Failure::blocked(
                "InputRequired",
                "the original upload coordinates are required",
            ));
        }
    }
    loc.decision(&ask)?;
    loc.announce()?;
    if matches!(ask, Ask::Decide(_, _, Verdict::Approve(_)))
        && let Some(display) = &plan.facts.approvals
    {
        if let Some(upload) = prepared.as_ref().and_then(|p| p.upload.as_ref()) {
            let expected = crate::launcher::deploy_foreign_lines(&upload.recipe, upload.root);
            if upload.root.0.as_slice() != display.r || !display.text.starts_with(&expected) {
                return Err(Failure::damaged(
                    "host display contradicts the attested recipe",
                ));
            }
        }
        loc.save_display(&display.r, &display.text)?;
    }
    if plan.facts.stage == DecisionStage::Creation
        && matches!(ask, Ask::Decide(_, _, Verdict::Approve(_)))
        && let Some(record) = plan.create.as_ref().and_then(|c| c.record.as_ref())
    {
        crate::launcher::private_dir(record.dir.clone().into()).map_err(Failure::io)?;
    }
    Exchange {
        target: &target,
        authority,
        d: &id,
        upload: prepared.as_ref().and_then(|p| p.upload.as_ref()),
        source: prepared.as_ref().and_then(|p| p.source.as_ref()),
        location: &loc,
        wait: wait.as_ref(),
        plan: false,
    }
    .run(&ask, env)
}
