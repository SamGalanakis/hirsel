//! Admission orchestration: the workers that admit the control session's attempts, the
//! input the session streams to them, and the notes they send back to the loop. Each
//! method answers the events it raises; the launcher queues them in order.
use super::admit::{self, Admission};
use super::pipe;
use super::ready::{READ, Readiness, Watch};
use super::session::{self, Approvals, Attempt, Event, Failure, Input};
use super::supervision::GroupAdmitState;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::sync::{Arc, mpsc};

/// Concurrent admissions; a further `Admit` fails until one ends.
const ADMISSIONS: usize = 4;

/// Admission state: the stores, the running workers, and the notes they send.
pub(super) struct Admissions {
    admission: Arc<Admission>,
    workers: BTreeMap<Attempt, admit::Worker>,
    next: u64,
    notes: mpsc::Receiver<admit::Note>,
    sender: mpsc::Sender<admit::Note>,
    wake: File,
    waker: Arc<File>,
    _watch: Watch,
}

pub(super) fn invalid(attempt: Attempt, why: impl Into<String>) -> Event {
    Event::AdmissionFailed(attempt, Failure::Invalid(why.into()))
}

impl Admissions {
    pub(super) fn new(poll: &Readiness, admission: Admission) -> io::Result<Self> {
        let (wake, write) = pipe()?;
        let mut watch = poll.watch(wake.as_raw_fd());
        watch.set(READ)?;
        let (sender, notes) = mpsc::channel();
        Ok(Self {
            admission: Arc::new(admission),
            workers: BTreeMap::new(),
            next: 0,
            notes,
            sender,
            wake,
            waker: admit::waker(write)?,
            _watch: watch,
        })
    }

    pub(super) fn admission(&self) -> Arc<Admission> {
        self.admission.clone()
    }

    /// Cancel `attempt`'s worker, if it runs.
    pub(super) fn cancel(&mut self, attempt: Attempt) {
        self.workers.remove(&attempt);
    }

    /// Cancel every worker: a new session ends the old one's admissions (§14.5).
    pub(super) fn clear(&mut self) {
        self.workers.clear();
    }

    fn failed(&mut self, attempt: Attempt, why: impl Into<String>) -> Event {
        self.workers.remove(&attempt);
        invalid(attempt, why)
    }

    /// Begin admitting `attempt` of a group in `state`; the event is its failure.
    pub(super) fn admit(
        &mut self,
        poll: &Readiness,
        state: GroupAdmitState,
        attempt: Attempt,
        input: Input,
        approvals: Approvals,
        deadline_ms: u64,
    ) -> Option<Event> {
        let refusal = match state {
            GroupAdmitState::Unknown => Some("unknown group"),
            GroupAdmitState::Retiring => Some("the group is retiring"),
            _ if self.workers.contains_key(&attempt) => Some("the attempt is admitting"),
            _ if self.workers.len() >= ADMISSIONS => Some("busy: every admission slot runs"),
            _ => None,
        };
        if let Some(why) = refusal {
            return Some(invalid(attempt, why));
        }
        let Some(deadline) = admit::deadline_from_wall_ms(deadline_ms) else {
            return Some(self.failed(attempt, "the admission deadline passed before Admit"));
        };
        let root = |r: &[u8]| {
            <[u8; 32]>::try_from(r)
                .ok()
                .map(hirsel_core_ids::ContentHash)
        };
        let source = match input {
            Input::Recovered(bytes) => match super::recovery::decode(&bytes) {
                Ok(plan) => Some(admit::Source::Recovered(plan)),
                Err(why) => return Some(invalid(attempt, why.to_string())),
            },
            Input::Attested(r) => root(&r).map(admit::Source::Attested),
            Input::Publication(r) => root(&r).map(admit::Source::Publication),
            Input::Source(bytes, gated) => match admit::source(&bytes, gated) {
                Ok(source) => Some(source),
                Err(why) => return Some(Event::AdmissionFailed(attempt, why)),
            },
        };
        let Some(source) = source else {
            return Some(invalid(attempt, "the recipe root is not 32 bytes"));
        };
        let id = self.next;
        self.next += 1;
        let (sender, waker) = (self.sender.clone(), self.waker.clone());
        let worker = admit::Worker::spawn(
            self.admission.clone(),
            poll,
            id,
            attempt,
            source,
            approvals,
            deadline,
            move |note| {
                let _ = sender.send(note);
                admit::wake(&waker);
            },
        );
        match worker {
            Ok(worker) => {
                self.workers.insert(attempt, worker);
                None
            }
            Err(e) => Some(self.failed(attempt, format!("admission worker: {e}"))),
        }
    }

    /// Queue `bytes` of `attempt`'s attested input; the event is its failure.
    pub(super) fn data(&mut self, attempt: Attempt, bytes: &[u8]) -> Option<Event> {
        let bound = self.admission.input_bound();
        let why = match self.workers.get_mut(&attempt) {
            None => "unknown attempt",
            Some(worker) if worker.ending => "input after AdmitEnd",
            Some(worker) => match &mut worker.conn {
                None => "a publication takes no input",
                Some(_) if worker.received.saturating_add(bytes.len() as u64) > bound => {
                    "admission input exceeds its bound"
                }
                Some(conn) => {
                    worker.received += bytes.len() as u64;
                    conn.output.extend_from_slice(bytes);
                    return None;
                }
            },
        };
        Some(self.failed(attempt, why))
    }

    pub(super) fn end(&mut self, attempt: Attempt) -> Option<Event> {
        match self.workers.get_mut(&attempt) {
            Some(worker) => {
                worker.ending = true;
                None
            }
            None => Some(self.failed(attempt, "unknown attempt")),
        }
    }

    pub(super) fn assemble(&mut self, attempt: Attempt) -> Option<Event> {
        match self.workers.get_mut(&attempt) {
            Some(worker) if worker.admitted => {
                worker.assemble();
                None
            }
            _ => Some(self.failed(attempt, "the attempt is not admitted")),
        }
    }

    /// Apply the workers' notes: their events, and each assembled record's key that its
    /// attempt now reserves. A note of a cancelled worker is dropped: its assembly record
    /// is then no reservation's and the next collection may remove it.
    pub(super) fn notes(&mut self) -> (Vec<Event>, Vec<(Attempt, [u8; 32])>) {
        let mut drained = [0; 64];
        while matches!((&self.wake).read(&mut drained), Ok(n) if n > 0) {}
        let mut events = Vec::new();
        let mut reserved = Vec::new();
        while let Ok(note) = self.notes.try_recv() {
            let (id, attempt) = match &note {
                admit::Note::Admitted { id, attempt, .. }
                | admit::Note::Assembled { id, attempt, .. }
                | admit::Note::Failed(id, attempt, _) => (*id, *attempt),
            };
            if self.workers.get(&attempt).is_none_or(|w| w.id != id) {
                continue;
            }
            match note {
                admit::Note::Admitted {
                    root,
                    summary,
                    display,
                    ..
                } => {
                    self.workers.get_mut(&attempt).unwrap().admitted = true;
                    events.push(Event::Admitted(attempt, root, summary, display));
                }
                admit::Note::Assembled {
                    key,
                    digest,
                    fetch,
                    assembly,
                    recovered,
                    ..
                } => {
                    self.workers.remove(&attempt);
                    reserved.push((attempt, key));
                    if let Some(proof) = recovered {
                        events.push(Event::RecoveredAssembly(
                            attempt,
                            key.to_vec(),
                            digest,
                            fetch,
                            assembly,
                            proof,
                        ));
                        continue;
                    }
                    events.push(Event::Assembled(
                        attempt,
                        key.to_vec(),
                        digest,
                        fetch,
                        assembly,
                    ));
                }
                admit::Note::Failed(_, _, why) => {
                    self.workers.remove(&attempt);
                    events.push(Event::AdmissionFailed(attempt, why));
                }
            }
        }
        (events, reserved)
    }

    /// Move each attested worker's input and Need.
    pub(super) fn pump(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for (&attempt, worker) in &mut self.workers {
            for (keys, last) in worker.pump() {
                events.push(Event::Need(attempt, keys, last));
            }
        }
        events
    }

    /// Whether some attested input waits on its worker past the window, so the session is
    /// not read until the worker takes it.
    pub(super) fn backlogged(&self) -> bool {
        self.workers.values().any(|w| {
            w.conn
                .as_ref()
                .is_some_and(|c| c.output.len() > session::MAX_FRAME)
        })
    }

    pub(super) fn watch(&mut self) -> io::Result<()> {
        for worker in self.workers.values_mut() {
            if let Some(conn) = &mut worker.conn {
                conn.watch()?;
            }
        }
        Ok(())
    }
}
