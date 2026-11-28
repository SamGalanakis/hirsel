//! Submit the external restore receipt as a fresh host registration. The
//! launcher verifies the claimed cut and retained image before exercising it.
use super::{
    config,
    context::Target,
    conversation::{Exchange, locate},
    finite::{Envelope, Failure},
    receipt::{Inputs, Location, Receipt, digest},
    wait::Wait,
};
use crate::{
    cli::{AdoptOption as O, Request},
    launcher::session::Wire,
    operator::{Answer, Ask, CatalogStore, Input, Recovery},
};
use hirsel_core_ids::hex;
use serde_json::{Value, json};
use std::{fs::File, path::Path};

fn id(raw: &str) -> Result<CatalogStore, Failure> {
    if raw.len() != 32 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Failure::input("receipt", "expected a 16-byte Store ID"));
    }
    let n = u128::from_str_radix(raw, 16).map_err(|e| Failure::input("receipt", e))?;
    Ok(CatalogStore {
        hi: (n >> 64) as u64,
        lo: n as u64,
    })
}
pub(super) fn evidence(p: &Recovery) -> Value {
    json!({"destination":p.directory,"child":format!("{:016x}{:016x}",p.child.hi,p.child.lo),"parent":format!("{:016x}{:016x}",p.parent.hi,p.parent.lo),
        "boundary":p.boundary,"branch":p.branch.to_string(),"version":p.cut.to_string(),"at_ms":p.at_ms.to_string(),
        "main_version":p.main_version.to_string(),"main_at_ms":p.main_at_ms.to_string(),"activation":p.activation.to_string(),
        "release_binding":p.release_binding,"attempt":p.attempt,"assembly":p.assembly.as_ref().map(|a|hex::encode(a))})
}
fn read(path: &Path, assembly: Option<Vec<u8>>) -> Result<Recovery, Failure> {
    let doc: Value = serde_json::from_reader(File::open(path).map_err(Failure::io)?)
        .map_err(|e| Failure::input("receipt", e))?;
    let r = &doc["result"];
    if doc["v"] != 1
        || doc["exit"] != 0
        || r["publication"] != "Published"
        || r["verification"] != "store_graph"
    {
        return Err(Failure::input(
            "receipt",
            "expected a published restore receipt",
        ));
    }
    let field = |k: &str| {
        r[k].as_str()
            .ok_or_else(|| Failure::input("receipt", format!("missing {k}")))
    };
    let counter = |k| {
        field(k)?
            .parse::<u64>()
            .map_err(|e| Failure::input("receipt", e))
    };
    let child = id(field("child")?)?;
    let parent = id(field("parent")?)?;
    let attempt = field("attempt")?;
    if child == parent
        || !hex::decode(attempt).is_some_and(|v| v.len() == 16)
        || doc["operation"]["id"] != format!("store/{attempt}")
    {
        return Err(Failure::input(
            "receipt",
            "invalid fresh-child identity or attempt",
        ));
    }
    let directory = field("destination")?;
    if !Path::new(directory).is_absolute() {
        return Err(Failure::input(
            "receipt",
            "expected an absolute Directory destination",
        ));
    }
    Ok(Recovery {
        directory: directory.to_owned(),
        parent,
        child,
        boundary: field("boundary")?.to_owned(),
        branch: counter("branch")?,
        cut: counter("version")?,
        at_ms: counter("at_ms")?,
        main_version: counter("main_version")?,
        main_at_ms: counter("main_at_ms")?,
        activation: counter("activation")?,
        release_binding: field("release_binding")?.to_owned(),
        attempt: attempt.to_owned(),
        assembly,
    })
}

pub(crate) fn run(req: Request<O>) -> i32 {
    let json = req.options.contains(&O::Json);
    let mut env = Envelope::read("host adopt");
    if let Err(e) = propose(req, &mut env) {
        env.fail(e)
    }
    env.render(json)
}
fn propose(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let (mut spec, mut context, mut request, mut wait, mut assembly) =
        (None, None, None, None, None);
    let mut plan = false;
    for opt in req.options {
        match opt {
            O::AdoptSpec(v) => spec = Some(v),
            O::Context(v) => context = Some(v),
            O::RequestId(v) => request = Some(v),
            O::Wait(w) => wait = Some(Wait::start(w)?),
            O::AdoptAssembly(v) => {
                assembly = Some(hex::decode(&v).filter(|b| b.len() == 32).ok_or_else(|| {
                    Failure::input("assembly", "expected a 32-byte assembly identity")
                })?)
            }
            O::Plan => plan = true,
            O::Json => (),
        }
    }
    env.contract = if plan {
        "plan"
    } else if wait.is_some() {
        "wait"
    } else {
        "submit"
    };
    let spec = spec.ok_or_else(|| Failure::input("as", "a host_v1.Group is required"))?;
    let path = req
        .positional
        .as_deref()
        .and_then(|p| p.strip_prefix('@'))
        .ok_or_else(|| Failure::input("receipt", "expected @RESTORE.json"))?;
    let proof = read(Path::new(path), assembly)?;
    let (reg, lines) = config::group(Path::new(&spec))?;
    if reg.store != proof.directory {
        return Err(Failure::input(
            "as",
            "group store differs from the restored destination",
        ));
    }
    let mut grants = crate::package_dir::Grants::default();
    for line in lines {
        super::grant(&mut grants, &line).map_err(|e| Failure::input("grants", e))?
    }
    let approvals = super::approvals(grants);
    let target = Target::resolve(context.as_deref())?;
    let authority = target.authority()?;
    let selection = super::observe::selection(&target, Some(&reg.name))?;
    let id = super::request_id(request).map_err(|e| Failure::input("request-id", e))?;
    let loc = Location::new(authority, &id)?;
    let subject = if let Some(r) = loc.read()? {
        r.precondition()?
    } else {
        match target.query(
            authority,
            &Ask::ObserveGroup(reg.name.clone()),
            wait.as_ref(),
        )? {
            Answer::Precondition(p) => p,
            Answer::Error(c, e) => return Err(Failure::authority(c, e)),
            _ => return Err(Failure::damaged("expected group precondition")),
        }
    };
    if subject.name != reg.name
        || subject.predecessor.is_some()
        || selection
            .and_then(|(_, i)| i)
            .is_some_and(|i| i != subject.generation)
    {
        return Err(Failure::input(
            "as",
            "adoption requires a fresh group at the selected name",
        ));
    }
    let pre = hex::encode(&subject.encode());
    let input = Input::Recovered(proof);
    let input_digest = digest(&input);
    let ask = Ask::Register(
        id.clone(),
        Box::new(reg),
        input,
        approvals.foreign,
        approvals.exec,
        subject,
        plan,
    );
    loc.save(&Receipt {
        id: loc.id.clone(),
        intent_digest: digest(&ask),
        input_digest,
        subject: pre,
        inputs: Inputs {
            create: Some(spec.into()),
            ..Default::default()
        },
    })?;
    loc.announce()?;
    locate(env, &target, &loc, authority, &id);
    Exchange {
        target: &target,
        authority,
        d: &id,
        upload: None,
        source: None,
        location: &loc,
        wait: wait.as_ref(),
        plan,
    }
    .run(&ask, env)
}
