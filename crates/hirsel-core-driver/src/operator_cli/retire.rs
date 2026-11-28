use super::{
    context::Target,
    conversation::{Exchange, locate},
    finite::{Envelope, Failure},
    receipt::{Inputs, Location, Receipt, digest},
};
use crate::{
    cli::{Request, RetireOption as O},
    launcher::session::Wire,
    operator::{Answer, Ask, Input},
};

pub(crate) fn run(req: Request<O>) -> i32 {
    let json = req.options.contains(&O::Json);
    let mut env = Envelope::read("host retire");
    env.contract = "plan";
    if let Err(failure) = propose(req, &mut env) {
        env.fail(failure)
    }
    env.render(json)
}
fn propose(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let (mut context, mut name, mut request) = (None, None, None);
    let mut seen = std::collections::HashSet::new();
    for o in req.options {
        if !seen.insert(std::mem::discriminant(&o)) {
            return Err(Failure::input("arguments", "duplicate option"));
        }
        match o {
            O::Json | O::Plan => (),
            O::Context(v) => context = Some(v),
            O::GroupName(v) => name = Some(v),
            O::RequestId(v) => request = Some(v),
        }
    }
    let target = Target::resolve(context.as_deref())?;
    let authority = target.authority()?;
    let Some((group, Some(incarnation))) = super::observe::selection(&target, name.as_deref())?
    else {
        return Err(Failure::input(
            "group",
            "select NAME@INCARNATION or a context pinned to an incarnation",
        ));
    };
    let id = super::request_id(request).map_err(|e| Failure::input("request-id", e))?;
    let loc = Location::new(authority, &id)?;
    locate(env, &target, &loc, authority, &id);
    let subject = if let Some(saved) = loc.read()? {
        saved.precondition()?
    } else {
        match target.query(authority, &Ask::ObserveGroup(group.into()), None)? {
            Answer::Precondition(p) => p,
            Answer::Error(c, detail) => return Err(Failure::authority(c, detail)),
            _ => return Err(Failure::damaged("expected a group precondition")),
        }
    };
    if subject.name != group || subject.generation != incarnation || subject.predecessor.is_none() {
        return Err(Failure::input(
            "incarnation",
            "the selected incarnation is absent",
        ));
    }
    let input_digest = digest(&Input::Publication(subject.assembly.clone()));
    let precondition = hirsel_core_ids::hex::encode(&subject.encode());
    let ask = Ask::Retire(id.clone(), subject);
    loc.save(&Receipt {
        id: loc.id.clone(),
        intent_digest: digest(&ask),
        input_digest,
        subject: precondition,
        inputs: Inputs::default(),
    })?;
    loc.announce()?;
    Exchange {
        target: &target,
        authority,
        d: &id,
        upload: None,
        source: None,
        location: &loc,
        wait: None,
        plan: true,
    }
    .run(&ask, env)
}
