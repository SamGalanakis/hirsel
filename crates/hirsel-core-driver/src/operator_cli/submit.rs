//! Bind deploy input to an observed incarnation or absence before saving and submitting it.
use super::{
    Approvals, Source, Upload, config,
    context::Target,
    conversation::{Exchange, locate},
    finite::{Envelope, Failure},
    receipt::{Inputs, Location, Receipt, digest},
    submission,
    wait::Wait,
};
use crate::{
    cli::{DeployOption as O, Request},
    launcher::session::{Group, Wire},
    operator::{Answer, Ask, Input, Registration},
};
use hirsel_core_ids::hex;
use std::path::{Path, PathBuf};

pub(super) struct Prepared {
    pub registration: Option<Registration>,
    pub input: Input,
    pub upload: Option<Upload>,
    pub source: Option<Source>,
    pub approvals: Approvals,
}
impl Inputs {
    pub fn open(&self, state: &Path) -> Result<Prepared, Failure> {
        let (registration, grants) = match &self.create {
            Some(path) => {
                let (r, g) = config::group(path)?;
                (Some(r), g)
            }
            None => (None, Vec::new()),
        };
        let (input, upload, source, mut approvals) = submission(
            self.source.clone(),
            self.recipe.as_deref().or(self.package.as_deref()),
            self.code.clone(),
            grants,
            state,
        )
        .map_err(|e| Failure::input("input", e))?;
        if let Some(u) = &upload {
            if registration.is_none() {
                approvals.foreign = crate::foreign_archive::native_bindings(&u.recipe)
                    .filter_map(|g| g.values.first().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                approvals.exec = crate::exec_launch::arguments(&u.recipe)
                    .filter_map(|s| s.strip_prefix("--hirsel-exec=").map(str::to_owned))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
            }
            if let Some(why) = approvals.check(&u.recipe) {
                return Err(Failure::input(
                    "grants",
                    why.split("; pass --grant").next().unwrap_or(&why),
                ));
            }
            if registration.as_ref().is_some_and(|g| g.record.is_some())
                && let Some(why) = crate::reachability::foreign_recording_refusal(&u.recipe)
            {
                return Err(Failure::input("recording", why));
            }
        }
        Ok(Prepared {
            registration,
            input,
            upload,
            source,
            approvals,
        })
    }
}
pub(crate) fn deploy(req: Request<O>) -> i32 {
    let json = req.options.contains(&O::Json);
    let mut env = Envelope::read(req.command.name);
    env.contract = "submit";
    if let Err(e) = submit(req, &mut env) {
        env.fail(e);
    }
    env.render(json)
}
fn submit(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let (mut context, mut name, mut request, mut wait) = (None, None, None, None);
    let rollback = if req.command.name == "rollback" {
        Some(
            req.positional
                .as_deref()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| {
                    Failure::input("activation", "select an activation as a decimal u64")
                })?,
        )
    } else {
        None
    };
    let mut inputs = Inputs {
        package: if rollback.is_none() {
            req.positional
        } else {
            None
        },
        rollback,
        source: None,
        create: None,
        recipe: None,
        code: None,
    };
    let mut plan = false;
    let mut seen = std::collections::HashSet::new();
    for o in req.options {
        if !seen.insert(std::mem::discriminant(&o)) {
            return Err(Failure::input("arguments", "duplicate option"));
        }
        let abs = |v: String| std::path::absolute(v).map_err(Failure::io);
        match o {
            O::Json => (),
            O::Plan => plan = true,
            O::Wait(w) => wait = Some(Wait::start(w)?),
            O::Context(v) => context = Some(v),
            O::GroupName(v) => name = Some(v),
            O::RequestId(v) => request = Some(v),
            O::Recipe(v) => inputs.recipe = Some(v),
            O::CodeStore(v) => inputs.code = Some(abs(v)?),
            O::Create(v) => inputs.create = Some(abs(v)?),
            O::SourceDir(v) => inputs.source = Some(abs(v)?),
        }
    }
    if inputs.recipe.is_some() && (inputs.package.is_some() || inputs.source.is_some())
        || inputs.code.is_some() && inputs.recipe.is_none()
    {
        return Err(Failure::input(
            "input",
            "choose DIR [--source ROOT] or --recipe R [--code-store DIR]",
        ));
    }
    if rollback.is_none() && inputs.recipe.is_none() && inputs.source.is_none() {
        inputs.source = Some(
            std::path::absolute(
                inputs
                    .package
                    .take()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| ".".into()),
            )
            .map_err(Failure::io)?,
        );
    }
    if inputs.recipe.is_some() && inputs.code.is_none() {
        inputs.code = Some(
            crate::store::default_path(&std::env::current_dir().map_err(Failure::io)?)
                .join("code-store"),
        );
    }
    env.contract = if plan {
        "plan"
    } else if wait.is_some() {
        "wait"
    } else {
        "submit"
    };
    let d = super::request_id(request).map_err(|e| Failure::input("request-id", e))?;
    if context.as_deref() == Some("dev") {
        return Err(Failure::input(
            "context",
            "the dev session does not admit live deployments",
        ));
    }
    let target = Target::resolve(context.as_deref())?;
    let authority = target.authority()?;
    let loc = Location::new(authority, &d)?;
    let prior = loc.read()?;
    if prior
        .as_ref()
        .is_some_and(|row| row.inputs.rollback != rollback)
    {
        return Err(Failure::input(
            "request-id",
            "request ID already names another intent",
        ));
    }
    let mut historical_subject = None;
    if let Some(activation) = rollback {
        let (selected, incarnation) = super::observe::selection(&target, name.as_deref())?
            .ok_or_else(|| Failure::input("group", "select a group"))?;
        if let Some(row) = &prior {
            inputs.recipe = row.inputs.recipe.clone();
            inputs.code = row.inputs.code.clone();
        } else {
            let (subject, root) = match target.query(
                authority,
                &Ask::Historical(selected.into(), incarnation, activation),
                wait.as_ref(),
            )? {
                Answer::Historical(p, root) => (p, root),
                Answer::Error(crate::operator::Class::UnknownGroup, _) => {
                    return Err(Failure::negative(
                        "UnknownGroup",
                        "no matching group incarnation",
                    ));
                }
                Answer::Error(crate::operator::Class::UnknownActivation, _) => {
                    return Err(Failure::negative(
                        "UnknownActivation",
                        "no supported release at this activation in the selected incarnation",
                    ));
                }
                Answer::Error(c, v) => return Err(Failure::authority(c, v)),
                _ => return Err(Failure::damaged("expected historical release")),
            };
            if subject.name != selected || incarnation.is_some_and(|i| i != subject.generation) {
                return Err(Failure::damaged("historical release names another group"));
            }
            let root = hirsel_core_ids::ContentHash(
                root.try_into()
                    .map_err(|_| Failure::damaged("invalid assembly identity"))?,
            );
            let state = target.state()?;
            let records = crate::launcher::objects::Records::read_only(&state.join("objects"))
                .map_err(Failure::io)?;
            let record = records.assembly(root).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    Failure::negative("ImageGone", "historical assembly is unavailable")
                }
                _ => Failure::io(e),
            })?;
            inputs.recipe = Some(record.recipe.to_string());
            inputs.code = Some(state.join("code"));
            historical_subject = Some(subject);
        }
    }
    let prepared = inputs.open(target.state()?)?;
    let selected = name
        .as_deref()
        .or_else(|| prepared.registration.as_ref().map(|r| r.name.as_str()));
    let (name, incarnation) = super::observe::selection(&target, selected)?
        .ok_or_else(|| Failure::input("group", "select a group"))?;
    if prepared
        .registration
        .as_ref()
        .is_some_and(|r| r.name != name)
    {
        return Err(Failure::input(
            "group",
            "group contradicts the selected target",
        ));
    }
    let subject = match (prior, historical_subject) {
        (Some(r), _) => r.precondition()?,
        (_, Some(p)) => p,
        (None, None) => {
            match target.query(authority, &Ask::ObserveGroup(name.into()), wait.as_ref())? {
                Answer::Precondition(p) => p,
                Answer::Error(c, v) => return Err(Failure::authority(c, v)),
                _ => return Err(Failure::damaged("expected group precondition")),
            }
        }
    };
    if incarnation.is_some_and(|id| id != subject.generation) {
        return Err(Failure::input("group", "group incarnation changed"));
    }
    if subject.name != name || subject.predecessor.is_some() == prepared.registration.is_some() {
        return Err(Failure::input(
            "group",
            "target does not match the requested group or expected absence",
        ));
    }
    target.check_group(&Group {
        name: subject.name.clone(),
        incarnation: subject.generation,
    })?;
    let pre = hex::encode(&subject.encode());
    let input_digest = digest(&prepared.input);
    let Approvals { foreign, exec } = prepared.approvals;
    let ask = match prepared.registration {
        Some(r) => Ask::Register(
            d.clone(),
            Box::new(r),
            prepared.input,
            foreign,
            exec,
            subject,
            plan,
        ),
        None => Ask::Begin(
            d.clone(),
            subject,
            prepared.input.into(),
            foreign,
            exec,
            plan,
        ),
    };
    loc.save(&Receipt {
        id: loc.id.clone(),
        intent_digest: digest(&ask),
        input_digest,
        subject: pre,
        inputs,
    })?;
    loc.announce()?;
    locate(env, &target, &loc, authority, &d);
    Exchange {
        target: &target,
        authority,
        d: &d,
        upload: prepared.upload.as_ref(),
        source: prepared.source.as_ref(),
        location: &loc,
        wait: wait.as_ref(),
        plan,
    }
    .run(&ask, env)
}
