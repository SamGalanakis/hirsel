//! A pinned operator exchange owns uploads and exact display confirmation until the host
//! can continue without its client. A deadline only detaches this observer.
use super::{
    CHUNK, Pump, Source, Upload,
    context::Target,
    finite::{Envelope, Failure, decimal_counters},
    receipt, state_json,
    wait::Wait,
};
use crate::launcher::session::Wire;
use crate::operator::{Answer, Ask, DeployPlan, Forward, Needed, Pause, Phase};
use hirsel_core_ids::hex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

pub(super) struct Exchange<'a> {
    pub target: &'a Target,
    pub authority: u128,
    pub d: &'a [u8],
    pub upload: Option<&'a Upload>,
    pub source: Option<&'a Source>,
    pub location: &'a receipt::Location,
    pub wait: Option<&'a Wait>,
    pub plan: bool,
}

pub(super) fn plan_json(p: &DeployPlan, hash: &[u8]) -> Value {
    let create=p.create.as_ref().map(|g| json!({
        "name":g.name,"tenant":g.tenant,"listen":g.listen,"workdir":g.workdir,"store":g.store,
        "trusted":g.trusted,"memory_bytes":g.memory.map(|v|v.to_string()),"args":g.args,
        "package_roots":g.packages.iter().map(|v|hex::encode(v)).collect::<Vec<_>>(),
        "recording":g.record.as_ref().map(|r|json!({"dir":r.dir,"retention_seconds":r.retention_s.to_string()})),
        "front":g.front.as_ref().map(|f|json!({"names":f.names,"forward":match f.forward{Forward::Http=>"http",Forward::Stream=>"stream"}})),
    }));
    let mut facts = super::decision_json(&p.facts);
    decimal_counters(&mut facts);
    let entries = p
        .entries
        .as_ref()
        .map(|v| {
            super::preview_json(&crate::operator::Preview {
                entries: v.clone(),
                ..Default::default()
            })["entries"]
                .clone()
        })
        .unwrap_or(json!("uncomputed"));
    if p.facts.migration.is_none() {
        facts["migration"] = json!("uncomputed");
    }
    if p.entries.is_none() {
        facts["triggers"] = json!("uncomputed");
    }
    json!({"domain":p.domain,"authority":format!("{:016x}{:016x}",p.authority.hi,p.authority.lo),"request":hex::encode(&p.request),"plan_digest":hex::encode(hash),"intent_digest":hex::encode(&p.intent_digest),"input_digest":hex::encode(&p.input_digest),"recovery":p.recovery.as_ref().map(super::adopt::evidence),
        "subject":{"name":p.subject.name,"generation":p.subject.generation.to_string(),"predecessor":p.subject.predecessor.map(|v|v.to_string()),"assembly":hex::encode(&p.subject.assembly),"store_id":p.subject.store_id.as_ref().map(|v|format!("{:016x}{:016x}",v.hi,v.lo)),"policy_digest":hex::encode(&p.subject.policy_digest)},
        "incarnation":p.incarnation.map(|i|i.to_string()),"create":create,"facts":facts,"entries":entries})
}

pub(super) fn validate(
    plan: &DeployPlan,
    hash: &[u8],
    authority: u128,
    d: &[u8],
) -> Result<(), Failure> {
    if plan.request != d
        || plan.domain
            != match plan.facts.stage {
                crate::operator::DecisionStage::Retirement => "hirsel.host.retire.plan.v1",
                _ => "hirsel.host.deploy.plan.v1",
            }
        || (u128::from(plan.authority.hi) << 64 | u128::from(plan.authority.lo)) != authority
        || Sha256::digest(plan.encode()).as_slice() != hash
    {
        return Err(Failure::damaged("invalid plan identity or digest"));
    }
    Ok(())
}
pub(super) fn current(
    target: &Target,
    authority: u128,
    d: &[u8],
    wait: Option<&Wait>,
) -> Result<(DeployPlan, Vec<u8>), Failure> {
    match target.query(authority, &Ask::Plan(d.to_vec()), wait)? {
        Answer::Plan(plan, hash) => {
            validate(&plan, &hash, authority, d)?;
            Ok((plan, hash))
        }
        Answer::Error(c, v) => Err(Failure::authority(c, v)),
        Answer::State(..) => Err(Failure::input(
            "operation",
            "operation has no outstanding decision",
        )),
        _ => Err(Failure::damaged("expected a held plan")),
    }
}
impl Exchange<'_> {
    pub fn run(&self, ask: &Ask, env: &mut Envelope<'_>) -> Result<(), Failure> {
        if let Some(w) = self.wait {
            w.remaining()?;
        }
        let mut reader = self
            .target
            .open(self.authority, self.wait.map(|w| w.observer))?;
        if self.wait.is_none() {
            reader.set_read_timeout(None).map_err(Failure::io)?;
        }
        let out = Arc::new(Mutex::new(reader.try_clone().map_err(Failure::io)?));
        if !self.plan {
            env.outcome["effect"] = json!("possible");
        }
        self.target.send(&mut reader, self.authority, ask)?;
        env.operation["evidence"] = json!("local_only");
        let mut pump: Option<Pump> = None;
        let mut blobs = Vec::new();
        let result = (|| loop {
            let ans = reader
                .recv()
                .map_err(Failure::io)?
                .ok_or_else(Failure::disconnected)?;
            match ans {
                Answer::Error(c, v) => {
                    env.outcome["effect"] = json!("none");
                    return Err(Failure::authority(c, v));
                }
                Answer::Plan(p, h) => {
                    validate(&p, &h, self.authority, self.d)?;
                    env.result = plan_json(&p, &h);
                    env.fail(Failure::input(
                        "plan_digest",
                        "the host returned a changed plan",
                    ));
                    env.outcome = json!({"kind":"PlanChanged","effect":"none"});
                    return Ok(());
                }
                Answer::Display(id, a, root, text) if id == self.d => {
                    if !self.location.display_approved(&root, &text)? {
                        return Err(Failure::damaged("display differs from the approved plan"));
                    }
                    let mut err = std::io::stderr().lock();
                    err.write_all(text.as_bytes())
                        .and_then(|()| err.flush())
                        .map_err(Failure::io)?;
                    out.lock()
                        .unwrap()
                        .send(&Ask::Confirm(
                            id,
                            a,
                            Sha256::digest(text.as_bytes()).to_vec(),
                        ))
                        .map_err(Failure::io)?;
                }
                Answer::Need(id, a, keys, last) if id == self.d => {
                    if self.source.is_some()
                        || keys.first().is_some_and(|k| matches!(k, Needed::Blob(_)))
                        || pump.is_none() && keys.is_empty()
                    {
                        for k in keys {
                            let Needed::Blob(h) = k else {
                                return Err(Failure::damaged("expected source blobs"));
                            };
                            blobs.push(h);
                        }
                        if last && !blobs.is_empty() {
                            let src = self.source.ok_or_else(|| {
                                Failure::blocked(
                                    "InputRequired",
                                    "resume the original source upload",
                                )
                            })?;
                            let bytes = src.stream(&blobs).map_err(Failure::damaged)?;
                            blobs.clear();
                            let mut conn = out.lock().unwrap();
                            for chunk in bytes.chunks(CHUNK) {
                                conn.send(&Ask::Data(id.clone(), a, chunk.to_vec()))
                                    .map_err(Failure::io)?;
                            }
                            conn.send(&Ask::End(id, a)).map_err(Failure::io)?;
                        }
                    } else {
                        let keys = keys
                            .into_iter()
                            .map(|k| match k {
                                Needed::Object(k) => Ok(k),
                                _ => Err(Failure::damaged("expected code objects")),
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        pump.as_mut()
                            .ok_or_else(|| Failure::damaged("no upload for Need"))?
                            .need(keys, last)
                            .map_err(Failure::io)?;
                    }
                }
                Answer::State(id, g, input, phase) if id == self.d => {
                    self.target.check_group(&g)?;
                    env.target["group"] = json!(g.name);
                    env.target["incarnation"] = json!(g.incarnation.to_string());
                    env.operation["evidence"] = json!("authority_catalog");
                    env.result = state_json(&id, &g, &phase);
                    decimal_counters(&mut env.result);
                    let (kind, exit, effect) = match phase {
                        Phase::AwaitInput(a)
                            if matches!(input, crate::operator::Input::Attested(_)) =>
                        {
                            if pump.is_none() {
                                let upload = self.upload.ok_or_else(|| {
                                    Failure::blocked(
                                        "InputRequired",
                                        "resume the original recipe upload",
                                    )
                                })?;
                                pump = Some(
                                    Pump::start(upload, &id, a, out.clone())
                                        .map_err(Failure::io)?,
                                );
                            }
                            continue;
                        }
                        Phase::AwaitDecision(_) => {
                            let (p, h) = current(self.target, self.authority, &id, self.wait)?;
                            env.result = plan_json(&p, &h);
                            env.next.push(
                                json!({"method":"op decide","operation":env.operation["id"]}),
                            );
                            if self.plan {
                                ("Planned", 0, "none")
                            } else {
                                ("AwaitDecision", 75, "none")
                            }
                        }
                        Phase::Installed(_, _, r) if matches!(r.pause, Pause::StartFailed(_)) => {
                            ("Failed", 1, "committed")
                        }
                        Phase::Retired => ("Completed", 0, "retired"),
                        Phase::AwaitStore(_) if self.wait.is_none() => ("Accepted", 0, "possible"),
                        Phase::Retiring if self.wait.is_none() => {
                            env.next
                                .push(json!({"method":"op get","operation":env.operation["id"]}));
                            ("Accepted", 0, "retiring")
                        }
                        Phase::Installed(..) => ("Completed", 0, "committed"),
                        Phase::Failed(..) => ("Failed", 1, "none"),
                        Phase::Undecided(..) => ("Undecided", 75, "possible"),
                        Phase::Assembling(_) | Phase::Preparing(_) | Phase::Pending(..)
                            if self.wait.is_none() && !self.plan =>
                        {
                            env.next
                                .push(json!({"method":"op get","operation":env.operation["id"]}));
                            ("Accepted", 0, "possible")
                        }
                        _ => continue,
                    };
                    env.exit = exit;
                    env.outcome = json!({"kind":kind,"effect":effect,"phase":env.result["phase"]});
                    return Ok(());
                }
                _ => return Err(Failure::damaged("unexpected reply or different operation")),
            }
        })();
        reader.shutdown();
        if let Some(pump) = pump {
            let _ = pump.stop();
        }
        result
    }
}
pub(super) fn locate(
    env: &mut Envelope<'_>,
    target: &Target,
    loc: &receipt::Location,
    authority: u128,
    d: &[u8],
) {
    env.target = json!(target);
    env.operation = json!({"kind":"host","authority":format!("{authority:032x}"),"request_id":hex::encode(d),"id":loc.id,"evidence":"local_only"});
}
