//! `db@1` places in this execution (STORE-12, runners.md `db@1`). A caller's
//! `runner.send` of a transact or query call becomes an arrival at the place a `sim@1`
//! node holds; the execution delivers it to the place's head run, which takes it from its
//! `Inbox` and ends it once. The caller's transact calls run on one call stream per place,
//! one request in flight; a query takes position 0. The head's revision is the caller's
//! program, and the place is reachable while its head runs: once the head ends, every call
//! it never took is `NotRun(Unreachable())` and every call it took and did not end is
//! `InDoubt(Abandoned())`.
use super::Library;
use super::dispatch::{arg, bytes, error, none, ok, some, u64_value, variant};
use super::simulation::Grant;
use crate::{
    ArrivedCall, BlockedWait, Error, HeadStart, OperationCall, OperationContext, OperationResult,
    PlaceArrival, Value, invalid,
};
use hirsel_run_schema::{CallForm, CallOutcome};
use std::collections::{BTreeMap, VecDeque};

const DENIED: u32 = 0;
const UNREACHABLE: u32 = 2;
const ABANDONED: u32 = 2;

#[derive(Default)]
pub(super) struct Places {
    /// Caller: arrivals for the execution to deliver.
    outgoing: Vec<PlaceArrival>,
    /// Caller: each sent call's wait and its end once it came.
    waits: BTreeMap<u64, Option<CallOutcome>>,
    /// Caller: each place's call stream, by runner name.
    streams: BTreeMap<String, Stream>,
    /// Head: the inbox's handle once granted; whether it was dropped.
    inbox: Option<u64>,
    closed: bool,
    /// Head: delivered arrivals not yet taken, taken ones by Arrival handle, and the ends
    /// given since the last take.
    queued: VecDeque<(u64, ArrivedCall)>,
    taken: BTreeMap<u64, (u64, ArrivedCall)>,
    ends: Vec<(u64, CallOutcome)>,
    /// Head: a parked `inbox.take(i, true)`.
    take_wait: Option<u64>,
}
/// A caller's transact stream at one place: the last position with a definite end, the
/// request in flight, and the transact calls waiting for it to end.
#[derive(Default)]
struct Stream {
    started: bool,
    applied: u64,
    in_flight: Option<u64>,
    waiting: VecDeque<PlaceArrival>,
}
impl Places {
    pub(super) fn waits_on(&self, token: u64) -> bool {
        self.waits.contains_key(&token)
    }
    pub(super) fn inbox_waits_on(&self, token: u64) -> bool {
        self.take_wait == Some(token)
    }
}

/// `H("hirsel.stream.v1", runner name, run instance)`, as a `db@1` caller's provider
/// names its stream.
fn stream_id(runner: &str, instance: &[u8; 16]) -> [u8; 32] {
    hirsel_run_schema::hash_parts("hirsel.stream.v1", [runner.as_bytes(), &instance[..]])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A `CallEnd` value.
pub(super) fn call_end(outcome: &CallOutcome) -> Value {
    match outcome {
        CallOutcome::Ok(b) => variant(0, vec![Value::Bytes(b.clone())]),
        CallOutcome::Failed(b) => variant(1, vec![Value::Bytes(b.clone())]),
        CallOutcome::Faulted(b) => variant(2, vec![Value::Bytes(b.clone())]),
        CallOutcome::NotRun(r) => variant(3, vec![variant(*r, vec![])]),
        CallOutcome::InDoubt(r) => variant(4, vec![variant(*r, vec![])]),
        CallOutcome::Canceled(c) => variant(5, vec![variant(*c, vec![])]),
    }
}
fn call_outcome(end: &Value) -> Result<CallOutcome, Error> {
    let Value::Variant { variant, fields } = end else {
        return Err(invalid("expected CallEnd"));
    };
    let field = fields
        .first()
        .and_then(Option::as_ref)
        .ok_or_else(|| invalid("dead CallEnd"))?;
    let reason = || match field {
        Value::Variant { variant, .. } => Ok(*variant),
        _ => Err(invalid("expected a reason")),
    };
    Ok(match variant {
        0 => CallOutcome::Ok(bytes(field)?.to_vec()),
        1 => CallOutcome::Failed(bytes(field)?.to_vec()),
        2 => CallOutcome::Faulted(bytes(field)?.to_vec()),
        3 => CallOutcome::NotRun(reason()?),
        4 => CallOutcome::InDoubt(reason()?),
        5 => CallOutcome::Canceled(reason()?),
        _ => return Err(invalid("CallEnd variant")),
    })
}
fn form_value(form: CallForm) -> Value {
    variant(
        match form {
            CallForm::Compute => 0,
            CallForm::Call => 1,
            CallForm::Transact => 2,
            CallForm::Query => 3,
        },
        vec![],
    )
}
fn record(name: &str, fields: Vec<Value>) -> Value {
    Value::Record {
        definition: (&hirsel_syntax::DefPath::core(name)).into(),
        fields: fields.into_iter().map(Some).collect(),
    }
}
fn arrived(call: &ArrivedCall) -> Value {
    record(
        "Arrived",
        vec![
            form_value(call.form),
            record(
                "EntryId",
                vec![
                    Value::String(call.path.clone()),
                    Value::Bytes(call.fingerprint.to_vec()),
                    Value::Bytes(call.code.to_vec()),
                ],
            ),
            Value::String(call.caller.clone()),
            record(
                "CallRequest",
                vec![
                    Value::Bytes(call.stream.to_vec()),
                    u64_value(call.epoch),
                    u64_value(call.position),
                    Value::Bytes(call.digest.to_vec()),
                    u64_value(call.acknowledged),
                ],
            ),
            match &call.level {
                Some(level) => some(Value::String(level.clone())),
                None => none(),
            },
            Value::Bytes(call.argument.clone()),
        ],
    )
}
fn list(elements: Vec<Value>) -> Value {
    Value::Sequence {
        kind: hirsel_types::BuiltinType::List,
        elements: elements.into_iter().map(Some).collect(),
        arena: None,
    }
}

impl Library {
    /// This run is a `db@1` place's head that a runner host serves: `io.inbox` grants its
    /// inbox once.
    pub fn serve_place(&mut self) {
        self.simulation.config.place = true;
    }

    pub(super) fn place_call(
        &mut self,
        name: &str,
        call: &OperationCall<'_>,
        context: &mut OperationContext<'_>,
    ) -> Result<OperationResult, Error> {
        let a = &*call.arguments;
        let value = match name {
            "runner.send" => return self.place_send(a, context),
            "io.inbox" => {
                self.simulation.id(arg(a, 0)?, "Io")?;
                if self.simulation.config.place && self.places.inbox.is_none() {
                    let handle = self.simulation.handle("Inbox", Grant::Root("Inbox".into()));
                    let Value::Handle { id, .. } = &handle else {
                        unreachable!()
                    };
                    self.places.inbox = Some(*id);
                    ok(handle)
                } else {
                    error(variant(0, vec![]))
                }
            }
            "inbox.take" => {
                self.simulation.id(arg(a, 0)?, "Inbox")?;
                let wait = matches!(arg(a, 1)?, Value::Bool(true));
                if self.places.queued.is_empty() && wait {
                    let token = self.next_callback;
                    self.next_callback += 1;
                    self.places.take_wait = Some(token);
                    return Ok(OperationResult::Park {
                        token,
                        wait: BlockedWait {
                            spawn_path: self.tasks.path(),
                            anchor: context
                                .anchor()
                                .cloned()
                                .ok_or_else(|| invalid("inbox.take without an anchor"))?,
                            kind: "inbox.take".into(),
                            targets: vec![],
                        },
                    });
                }
                self.taken()
            }
            "arrival.request" => {
                let id = self.simulation.id(arg(a, 0)?, "Arrival")?;
                let (_, call) = self
                    .places
                    .taken
                    .get(&id)
                    .ok_or_else(|| invalid("an Arrival already ended"))?;
                arrived(call)
            }
            "arrival.end" => {
                let id = self.simulation.id(arg(a, 0)?, "Arrival")?;
                let outcome = call_outcome(arg(a, 1)?)?;
                let (arrival, _) = self
                    .places
                    .taken
                    .remove(&id)
                    .ok_or_else(|| invalid("an Arrival ended twice"))?;
                self.simulation.grants.remove(&id);
                self.places.ends.push((arrival, outcome));
                Value::Unit
            }
            _ => return Err(invalid("unknown place operation")),
        };
        Ok(OperationResult::returned(value))
    }

    /// The queued arrivals, each as an `Arrival` handle.
    fn taken(&mut self) -> Value {
        let queued = std::mem::take(&mut self.places.queued);
        let mut handles = vec![];
        for (arrival, call) in queued {
            let handle = self
                .simulation
                .handle("Arrival", Grant::Root("Arrival".into()));
            let Value::Handle { id, .. } = &handle else {
                unreachable!()
            };
            self.places.taken.insert(*id, (arrival, call));
            handles.push(handle);
        }
        some(list(handles))
    }

    pub(super) fn poll_inbox(&mut self, token: u64) -> Result<Option<OperationResult>, Error> {
        if self.places.queued.is_empty() && !self.places.closed {
            return Ok(None);
        }
        if self.places.take_wait == Some(token) {
            self.places.take_wait = None;
        }
        Ok(Some(OperationResult::returned(self.taken())))
    }

    pub(super) fn poll_place(&mut self, token: u64) -> Result<Option<OperationResult>, Error> {
        match self.places.waits.get(&token) {
            Some(Some(_)) => {
                let outcome = self.places.waits.remove(&token).flatten().unwrap();
                Ok(Some(OperationResult::returned(call_end(&outcome))))
            }
            Some(None) => Ok(None),
            None => Err(invalid("unknown place wait")),
        }
    }

    /// `runner.send(r, form, f, argument)`: the host's checks, then the call on its
    /// stream to the place.
    fn place_send(
        &mut self,
        a: &[Value],
        context: &mut OperationContext<'_>,
    ) -> Result<OperationResult, Error> {
        self.tasks.yield_now = true;
        let runner = self.runner(arg(a, 0)?)?.clone();
        let Value::Variant { variant: form, .. } = arg(a, 1)? else {
            return Err(invalid("expected CallForm"));
        };
        let form = match form {
            2 => CallForm::Transact,
            3 => CallForm::Query,
            _ => CallForm::Compute,
        };
        let Value::FunctionReference(entry) = arg(a, 2)? else {
            return Err(invalid("runner.send names an Entry"));
        };
        let path = entry.definition.to_string();
        let argument = bytes(arg(a, 3)?)?.to_vec();
        let entry_model = &runner.entry;
        let place =
            entry_model.models.as_deref() == Some("db@1") && entry_model.provider == "sim@1";
        let listed = runner
            .definitions
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&path));
        let head = entry_model
            .head
            .as_deref()
            .and_then(hirsel_syntax::DefPath::from_full);
        let (Some(head), true, true) = (head, place && form.served(), listed) else {
            return Ok(OperationResult::returned(call_end(&CallOutcome::NotRun(
                DENIED,
            ))));
        };
        if let Some(cause) = self.tasks.canceled_task(self.tasks.current) {
            return Ok(OperationResult::returned(call_end(&CallOutcome::Canceled(
                cause,
            ))));
        }
        let facts = self.simulation.config.callees.get(&path).cloned();
        let (fingerprint, code) = facts
            .as_ref()
            .map_or(([0; 32], [0; 32]), |f| (f.fingerprint, f.code));
        let instance = self.simulation.config.instance;
        let name = entry_model.name.clone();
        let stream = self.places.streams.entry(name.clone()).or_default();
        let head = if stream.started {
            None
        } else {
            stream.started = true;
            let node = entry_model.node.as_deref().unwrap_or(&name);
            let seed = |part: &str| {
                let h = hirsel_run_schema::hash_parts(
                    "hirsel.place.v1",
                    [node.as_bytes(), part.as_bytes()],
                );
                u64::from_le_bytes(h[..8].try_into().unwrap())
            };
            let configuration = hirsel_tir::RunConfiguration {
                entry: head,
                hash_seed: seed("hash"),
                inputs: vec![],
            };
            let mut place_instance = [0; 16];
            place_instance.copy_from_slice(
                &hirsel_run_schema::hash_parts("hirsel.place.v1", [node.as_bytes()])[..16],
            );
            let mut world = super::runners::callee_world(
                &runner.grants,
                seed("random"),
                seed("test"),
                place_instance,
                self.simulation.config.fault_seed,
                self.simulation.config.callees.clone(),
            );
            world.place = true;
            let mut library = Library::new(&configuration, world)?;
            let io = library.simulation.handle("Io", Grant::Root("Io".into()));
            Some(HeadStart {
                configuration,
                arguments: vec![io, Value::Unit],
                operations: Box::new(library),
            })
        };
        let position = match form {
            CallForm::Transact => stream.applied + 1,
            _ => 0,
        };
        let token = self.next_callback;
        self.next_callback += 1;
        // Runners naming one node reach one place, each on its own stream.
        let place = entry_model.node.clone().unwrap_or_else(|| name.clone());
        let arrival = PlaceArrival {
            place,
            token,
            head,
            call: ArrivedCall {
                form,
                digest: hirsel_run_schema::request_digest(
                    form,
                    &path,
                    &fingerprint,
                    &code,
                    &[],
                    &[],
                    &argument,
                ),
                path,
                fingerprint,
                code,
                caller: hex(&instance),
                stream: stream_id(&name, &instance),
                epoch: 1,
                position,
                acknowledged: stream.applied,
                level: entry_model.level.clone(),
                argument,
            },
        };
        self.places.waits.insert(token, None);
        if form == CallForm::Transact {
            if stream.in_flight.is_some() {
                // One request in flight per stream: this one waits for its position.
                stream.waiting.push_back(arrival);
            } else {
                stream.in_flight = Some(token);
                self.places.outgoing.push(arrival);
            }
        } else {
            self.places.outgoing.push(arrival);
        }
        Ok(OperationResult::Park {
            token,
            wait: BlockedWait {
                spawn_path: self.tasks.path(),
                anchor: context
                    .anchor()
                    .cloned()
                    .ok_or_else(|| invalid("runner.send without an anchor"))?,
                kind: "runner.send".into(),
                targets: vec![],
            },
        })
    }

    pub(super) fn take_place_arrivals(&mut self) -> Vec<PlaceArrival> {
        std::mem::take(&mut self.places.outgoing)
    }

    pub(super) fn deliver_place_arrival(&mut self, id: u64, call: ArrivedCall) {
        if self.places.closed {
            self.places
                .ends
                .push((id, CallOutcome::NotRun(UNREACHABLE)));
        } else {
            self.places.queued.push_back((id, call));
        }
    }

    pub(super) fn take_place_ends(&mut self) -> Vec<(u64, CallOutcome)> {
        std::mem::take(&mut self.places.ends)
    }

    /// The head ended: the calls it never took never ran; those it took are in doubt.
    pub(super) fn close_place(&mut self) -> Vec<(u64, CallOutcome)> {
        let mut ends = std::mem::take(&mut self.places.ends);
        ends.extend(
            std::mem::take(&mut self.places.queued)
                .into_iter()
                .map(|(id, _)| (id, CallOutcome::NotRun(UNREACHABLE))),
        );
        ends.extend(
            std::mem::take(&mut self.places.taken)
                .into_values()
                .map(|(id, _)| (id, CallOutcome::InDoubt(ABANDONED))),
        );
        ends
    }

    /// A call sent at `token` ended: its stream moves past a definite end, and the next
    /// waiting transact call is sent.
    pub(super) fn place_ended(&mut self, token: u64, end: CallOutcome) -> Result<(), Error> {
        let wait = self
            .places
            .waits
            .get_mut(&token)
            .ok_or_else(|| invalid("an end for an unknown place call"))?;
        let mut next = None;
        for stream in self.places.streams.values_mut() {
            if stream.in_flight != Some(token) {
                continue;
            }
            // A recorded end, or one that may have committed, consumes its position.
            if matches!(
                end,
                CallOutcome::Ok(_) | CallOutcome::Failed(_) | CallOutcome::InDoubt(_)
            ) {
                stream.applied += 1;
            }
            stream.in_flight = None;
            if let Some(mut arrival) = stream.waiting.pop_front() {
                arrival.call.position = stream.applied + 1;
                arrival.call.acknowledged = stream.applied;
                stream.in_flight = Some(arrival.token);
                next = Some(arrival);
            }
        }
        *wait = Some(end);
        self.places.outgoing.extend(next);
        Ok(())
    }

    pub(super) fn drop_arrival(&mut self, id: u64) {
        if let Some((arrival, _)) = self.places.taken.remove(&id) {
            self.places
                .ends
                .push((arrival, CallOutcome::InDoubt(ABANDONED)));
        }
    }

    pub(super) fn drop_inbox(&mut self) {
        self.places.closed = true;
        let queued = std::mem::take(&mut self.places.queued);
        self.places.ends.extend(
            queued
                .into_iter()
                .map(|(id, _)| (id, CallOutcome::NotRun(UNREACHABLE))),
        );
    }
}
