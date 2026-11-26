//! Interpret the generated operator replies for one submission.
use super::{
    Approvals, CHUNK, Error, Pump, Source, UNAVAILABLE, Upload, class_name, connect, state_json,
};
use crate::code_admission::ObjectKey;
use crate::launcher::session::{Attempt, Grants, Group};
use crate::operator::{Answer, Ask, Class, DecisionStage, Fates, Needed, Pause, Phase, Verdict};
use hirsel_core_ids::hex;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BUSY_WAIT: Duration = Duration::from_secs(60);

/// A Foreign or Exec policy refusal, which exits 2 like a refusal the CLI finds itself.
fn policy_refusal(detail: &str) -> bool {
    detail.starts_with("FFI-8: ")
        || detail.starts_with("Exec[")
        || detail.starts_with("Exec grants")
}

/// One deploy or registration: its request, the upload, the display and the outcome.
pub(super) struct Conversation<'a> {
    state: &'a Path,
    d: Vec<u8>,
    command: &'static str,
    upload: Option<Upload>,
    approvals: Approvals,
    /// The grant diff the operator approves, sent as a Decide when the request awaits an
    /// Admission-stage decision on exactly it.
    pub(super) decision: Option<Grants>,
    /// The source tree whose blobs the host's `Need` asks for, when the input is a manifest.
    pub(super) source: Option<Source>,
}

impl<'a> Conversation<'a> {
    pub(super) fn new(
        state: &'a Path,
        d: Vec<u8>,
        command: &'static str,
        upload: Option<Upload>,
        approvals: Approvals,
    ) -> Self {
        Self {
            state,
            d,
            command,
            upload,
            approvals,
            decision: None,
            source: None,
        }
    }

    pub(super) fn run(self, ask: Ask) -> Result<i32, Error> {
        let started = std::time::Instant::now();
        loop {
            match self.attach(&ask)? {
                Some(code) => return Ok(code),
                None if started.elapsed() < BUSY_WAIT => {
                    std::thread::sleep(Duration::from_millis(250))
                }
                None => {
                    eprintln!(
                        "{}: the host stayed busy; resend request {}",
                        self.command,
                        hex::encode(&self.d)
                    );
                    return Ok(UNAVAILABLE);
                }
            }
        }
    }

    /// One attachment to the request: its exit status, or None when the host is busy and
    /// the same request should be sent again.
    fn attach(&self, ask: &Ask) -> Result<Option<i32>, Error> {
        let mut reader = connect(self.state)?;
        let out = Arc::new(Mutex::new(reader.try_clone()?));
        out.lock().unwrap().send(ask)?;
        let mut input = self
            .source
            .as_ref()
            .map_or(Input::Recipe, |source| Input::Source {
                source,
                needed: Vec::new(),
            });
        let mut pump = None;
        let mut approval = if matches!(ask, Ask::Decide(..)) {
            None
        } else {
            self.decision.as_ref()
        };
        let result = (|| loop {
            let answer = match reader.recv() {
                Ok(Some(answer)) => answer,
                Ok(None) | Err(_) => break Ok(Some(self.lost())),
            };
            match input.reply(answer, &mut approval, &self.d)? {
                Reply::Continue => {}
                Reply::StartUpload(attempt) => {
                    if pump.is_none() {
                        let upload = self.upload.as_ref().ok_or_else(|| format!(
                            "request {} uploads its input; resend it with `hirsel deploy R --request-id HEX`",
                            hex::encode(&self.d)
                        ))?;
                        pump = Some(Pump::start(upload, &self.d, attempt, out.clone())?);
                    }
                }
                Reply::Blobs(source, attempt, wanted) => {
                    let stream = source.stream(&wanted)?;
                    if stream.is_empty() {
                        continue;
                    }
                    let mut connection = out.lock().unwrap();
                    let sent = stream
                        .chunks(CHUNK)
                        .try_for_each(|chunk| {
                            connection.send(&Ask::Data(self.d.clone(), attempt, chunk.to_vec()))
                        })
                        .and_then(|()| connection.send(&Ask::End(self.d.clone(), attempt)));
                    if sent.is_err() {
                        break Ok(Some(self.lost()));
                    }
                }
                Reply::Objects(keys, last) => {
                    if let Some(pump) = &mut pump
                        && pump.need(keys, last).is_err()
                    {
                        break Err("the local upload ended before the host's Need".into());
                    }
                }
                Reply::Display(attempt, root, text) => {
                    self.check_display(&root, &text)?;
                    let mut stdout = io::stdout().lock();
                    stdout.write_all(text.as_bytes())?;
                    stdout.flush()?;
                    let hash = Sha256::digest(text.as_bytes()).to_vec();
                    let ask = Ask::Confirm(self.d.clone(), attempt, hash);
                    if out.lock().unwrap().send(&ask).is_err() {
                        break Ok(Some(self.lost()));
                    }
                }
                Reply::Decide(ask) => {
                    if out.lock().unwrap().send(&ask).is_err() {
                        break Ok(Some(self.lost()));
                    }
                }
                Reply::Report {
                    d,
                    group,
                    phase,
                    status,
                } => {
                    match &*phase {
                        Phase::AwaitDecision(_) => eprintln!(
                            "{}: request {} awaits the operator's decision; decide it with `hirsel decide --state DIR --request-id {}`",
                            self.command,
                            hex::encode(&self.d),
                            hex::encode(&self.d)
                        ),
                        Phase::Failed(class, detail, _) => {
                            eprintln!("{}: {}: {detail}", self.command, class_name(*class));
                        }
                        _ => {}
                    }
                    println!("{}", state_json(&d, &group, &phase));
                    break Ok(Some(status));
                }
                Reply::Failed(class, detail) => {
                    eprintln!("{}: {}: {detail}", self.command, class_name(class));
                    break Ok(Some(1));
                }
                Reply::Busy => break Ok(None),
            }
        })();
        reader.shutdown();
        if let Some(pump) = pump {
            let _ = pump.stop();
        }
        result
    }

    fn lost(&self) -> i32 {
        eprintln!(
            "{}: outcome unknown: request {}",
            self.command,
            hex::encode(&self.d)
        );
        UNAVAILABLE
    }

    /// The display must name exactly the submitted recipe's Foreign bindings, and Exec
    /// lines only when the operator approved Exec requests.
    /// A source's H is compiled by the host, which the CLI cannot recompute, so a source's
    /// display is shown unchecked; the host still admits only the approvals sent.
    fn check_display(&self, root: &[u8], text: &str) -> Result<(), Error> {
        let Some(upload) = &self.upload else {
            return Ok(());
        };
        if root != upload.root.0 {
            return Err("the host displayed approvals for another recipe".into());
        }
        let foreign = crate::launcher::deploy_foreign_lines(&upload.recipe, upload.root);
        let exec = text
            .strip_prefix(&foreign)
            .ok_or("the host reported different Foreign bindings")?;
        if !exec.is_empty() && self.approvals.exec.is_empty() {
            return Err("the host displayed Exec approvals the operator did not request".into());
        }
        Ok(())
    }
}

/// The request determines which kind of Need the host may send.
enum Input<'a> {
    Recipe,
    Source {
        source: &'a Source,
        needed: Vec<[u8; 32]>,
    },
}

enum Reply<'a> {
    Continue,
    StartUpload(Attempt),
    Blobs(&'a Source, Attempt, Vec<[u8; 32]>),
    Objects(Vec<ObjectKey>, bool),
    Display(Attempt, Vec<u8>, String),
    Decide(Ask),
    Report {
        d: Vec<u8>,
        group: Group,
        phase: Box<Phase>,
        status: i32,
    },
    Failed(Class, String),
    Busy,
}

impl<'a> Input<'a> {
    fn reply(
        &mut self,
        answer: Answer,
        approval: &mut Option<&Grants>,
        request: &[u8],
    ) -> Result<Reply<'a>, Error> {
        match answer {
            Answer::State(d, group, _, phase) => {
                let status = match &phase {
                    Phase::AwaitInput(_) if matches!(self, Self::Source { .. }) => {
                        return Ok(Reply::Continue);
                    }
                    Phase::AwaitInput(attempt) => return Ok(Reply::StartUpload(*attempt)),
                    Phase::AwaitDecision(held) => {
                        if held.stage == DecisionStage::Admission
                            && !held.shows_held()
                            && *approval == Some(&held.grants)
                        {
                            approval.take();
                            return Ok(Reply::Decide(Ask::Decide(
                                request.to_vec(),
                                held.clone(),
                                Verdict::Approve(Fates::default()),
                            )));
                        }
                        2
                    }
                    Phase::Undecided(..) => UNAVAILABLE,
                    Phase::Installed(_, _, report) => match report.pause {
                        Pause::StartFailed(_) => 1,
                        _ => 0,
                    },
                    Phase::Failed(Class::Denied, detail, _) if policy_refusal(detail) => 2,
                    Phase::Failed(..) => 1,
                    _ => return Ok(Reply::Continue),
                };
                Ok(Reply::Report {
                    d,
                    group,
                    phase: Box::new(phase),
                    status,
                })
            }
            Answer::Need(_, attempt, keys, last) => match self {
                Self::Source { source, needed } => {
                    for key in keys {
                        let Needed::Blob(digest) = key else {
                            return Err("the host asked a source upload for code objects".into());
                        };
                        needed.push(digest);
                    }
                    if last {
                        Ok(Reply::Blobs(source, attempt, std::mem::take(needed)))
                    } else {
                        Ok(Reply::Continue)
                    }
                }
                Self::Recipe => {
                    let objects = keys
                        .into_iter()
                        .map(|key| match key {
                            Needed::Object(key) => Ok(key),
                            Needed::Blob(_) => {
                                Err("the host asked a recipe upload for source blobs")
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Reply::Objects(objects, last))
                }
            },
            Answer::Display(_, attempt, root, text) => Ok(Reply::Display(attempt, root, text)),
            Answer::Error(Class::Busy, _) => Ok(Reply::Busy),
            Answer::Error(class, detail) => Ok(Reply::Failed(class, detail)),
            other => Err(format!("unexpected reply {other:?}").into()),
        }
    }
}
