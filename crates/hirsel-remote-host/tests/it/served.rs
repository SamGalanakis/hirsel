//! Served calls at a `db@1` place (runners.md `db@1`): the host makes its share of the
//! RUN-8 checks, hands what passes to the head, and sends the end the head reports to the
//! stream's current session. The law: every served call gets exactly one end, on the
//! session that holds its stream when the end exists; the host never decides a served
//! position, so a reopened stream's `applied` does not move.
use hirsel_remote::frame::{Call, Frame, Outcome};
use hirsel_remote::wire::Digests;
use hirsel_remote_host::host::{
    ABANDONED, Action, Arrival, Caller, DENIED, Host, Limits, OVERLOADED, Place, REJECTED, Refusal,
    UNREACHABLE,
};
use hirsel_remote_host::journal::NoJournal;
use hirsel_run_schema::{CallForm, CallOutcome, GrantSet, IoGrants, call_key};

const STREAM: [u8; 32] = [7; 32];
const INSTANCE: [u8; 16] = [3; 16];

struct Digest;
impl Digests for Digest {
    fn digest(&self, call: &Call) -> [u8; 32] {
        [call.position as u8; 32]
    }
}
/// A place that refuses served calls to paths ending `denied`.
struct Head;
impl Place for Head {
    fn admit(&mut self, _: &Caller, _: &Call) -> Result<IoGrants, Refusal> {
        Ok(IoGrants::default())
    }
    fn admit_served(&mut self, _: &Caller, call: &Call) -> Result<(), Refusal> {
        if call.path.ends_with("denied") {
            return Err(Refusal {
                reason: DENIED,
                missing: vec![],
            });
        }
        Ok(())
    }
}

fn host() -> Host {
    Host::new(
        Box::new(NoJournal::new([1; 16])),
        Limits {
            runs: 1,
            queue: 1,
            streams: 4,
            unacknowledged: 4,
        },
    )
    .unwrap()
}
fn open(host: &mut Host, session: u64, epoch: u64) -> Vec<Action> {
    host.connected(
        session,
        Some(Caller {
            name: "app".into(),
            grants: IoGrants::default(),
            definitions: None,
        }),
    );
    host.frame(
        session,
        Frame::Open {
            stream: STREAM,
            instance: INSTANCE,
            epoch,
        },
        &mut Head,
        &Digest,
    )
}
fn served(epoch: u64, position: u64, path: &str) -> Frame {
    Frame::Call(Box::new(Call {
        epoch,
        position,
        ordinal: position,
        key: call_key(&INSTANCE, position),
        form: CallForm::Transact,
        path: path.into(),
        fingerprint: [0; 32],
        code: [0; 32],
        grants: GrantSet::default(),
        definitions: None,
        argument: vec![position as u8],
        acknowledged: 0,
        level: None,
    }))
}
/// The ends `actions` send, by session and position.
fn ends(actions: &[Action]) -> Vec<(u64, u64, CallOutcome)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Send(session, Frame::Outcome(o)) => Some((*session, o.position, o.end.clone())),
            _ => None,
        })
        .collect()
}
fn arrivals(actions: Vec<Action>) -> Vec<Arrival> {
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::Arrive(arrival) => Some(*arrival),
            _ => None,
        })
        .collect()
}

#[test]
fn a_served_end_reaches_the_streams_current_session_once() {
    let mut host = host();
    open(&mut host, 1, 1);
    let arrived = arrivals(host.frame(1, served(1, 1, "app.pay"), &mut Head, &Digest));
    let [arrival] = &arrived[..] else {
        panic!("one arrival: {arrived:?}")
    };
    assert_eq!(
        (
            arrival.caller.as_str(),
            arrival.stream,
            arrival.call.argument.clone()
        ),
        ("app", STREAM, vec![1])
    );
    // The caller's session is lost and a new one opens the stream before the head ends.
    host.disconnected(1);
    let reopened = open(&mut host, 2, 2);
    assert!(
        reopened
            .iter()
            .any(|a| matches!(a, Action::Send(2, Frame::Opened { applied: 0, .. }))),
        "the host decides no served position: {reopened:?}"
    );
    let actions = host.arrival_ended(arrival.id, CallOutcome::Ok(vec![9]));
    let [Action::Send(2, Frame::Outcome(outcome))] = &actions[..] else {
        panic!("one outcome to the new session: {actions:?}")
    };
    assert_eq!(
        **outcome,
        Outcome {
            epoch: 2,
            position: 1,
            key: call_key(&INSTANCE, 1),
            digest: [1; 32],
            end: CallOutcome::Ok(vec![9]),
            callee: None,
            missing: vec![],
        }
    );
    assert!(
        host.arrival_ended(arrival.id, CallOutcome::Ok(vec![9]))
            .is_empty()
    );
}

#[test]
fn the_hosts_checks_refuse_before_the_head_and_a_closed_place_is_unreachable() {
    let mut host = host();
    open(&mut host, 1, 1);
    // A key that is not the stream instance's for its ordinal.
    let Frame::Call(mut forged) = served(1, 1, "app.pay") else {
        unreachable!()
    };
    forged.key = [0; 32];
    let refused = host.frame(1, Frame::Call(forged), &mut Head, &Digest);
    assert_eq!(ends(&refused), [(1, 1, CallOutcome::NotRun(REJECTED))]);
    assert!(arrivals(refused).is_empty());
    let refused = host.frame(1, served(1, 1, "app.denied"), &mut Head, &Digest);
    assert_eq!(ends(&refused), [(1, 1, CallOutcome::NotRun(DENIED))]);
    // runs + queue served calls in flight, then the next is overloaded.
    let mut held = vec![];
    for position in 1..=2 {
        held.extend(arrivals(host.frame(
            1,
            served(1, position, "app.pay"),
            &mut Head,
            &Digest,
        )));
    }
    assert_eq!(held.len(), 2);
    let refused = host.frame(1, served(1, 3, "app.pay"), &mut Head, &Digest);
    assert_eq!(ends(&refused), [(1, 3, CallOutcome::NotRun(OVERLOADED))]);
    // The head ends after ending one call: the other is in doubt; later calls never run.
    host.arrival_ended(held[0].id, CallOutcome::Ok(vec![]));
    assert_eq!(
        ends(&host.head_ended()),
        [(1, 2, CallOutcome::InDoubt(ABANDONED))]
    );
    let refused = host.frame(1, served(1, 3, "app.pay"), &mut Head, &Digest);
    assert_eq!(ends(&refused), [(1, 3, CallOutcome::NotRun(UNREACHABLE))]);
}

/// The law from the caller's side: a caller's provider keeps the served positions (the
/// host keeps none), one transact in flight; when a served end is lost with its
/// connection, the provider sends the call again at its position and the store answers
/// it from its stream rows, so every transact runs once and ends `Ok` once, at positions
/// 1, 2, ... in submission order.
#[test]
fn a_lost_served_end_is_resolved_by_sending_the_call_again_at_its_position() {
    use hirsel_remote::client::{Client, ClientAction, Settings};
    use hirsel_run_schema::RunnerCompletion;
    use std::collections::BTreeMap;
    /// The place's store: each position runs once, and a repeated position is answered
    /// from its recorded row.
    #[derive(Default)]
    struct Store {
        rows: BTreeMap<u64, CallOutcome>,
        runs: Vec<u64>,
    }
    impl Store {
        fn end(&mut self, arrival: &Arrival) -> CallOutcome {
            let position = arrival.call.position;
            self.rows
                .entry(position)
                .or_insert_with(|| {
                    self.runs.push(position);
                    CallOutcome::Ok(arrival.call.argument.clone())
                })
                .clone()
        }
    }
    let call = |ordinal: u64| Call {
        epoch: 0,
        position: 0,
        ordinal,
        key: call_key(&INSTANCE, ordinal),
        form: CallForm::Transact,
        path: "app.pay".into(),
        fingerprint: [0; 32],
        code: [0; 32],
        grants: GrantSet::default(),
        definitions: None,
        argument: vec![10 + ordinal as u8],
        acknowledged: 0,
        level: None,
    };
    let mut host = host();
    let mut store = Store::default();
    let mut client = Client::new(
        STREAM,
        INSTANCE,
        Settings {
            reconnect: 1,
            ..Settings::default()
        },
    );
    let mut completed = vec![];
    let mut session = 0;
    // Carry the provider's actions to the host and back until both are quiet; the head
    // ends each arrival at once. With `lose`, the first served end is lost with the
    // connection that carried it.
    let mut exchange = |client: &mut Client,
                        host: &mut Host,
                        store: &mut Store,
                        session: &mut u64,
                        mut from_client: Vec<ClientAction>,
                        mut lose: bool| {
        while !from_client.is_empty() {
            let mut to_client = vec![];
            for action in from_client.drain(..) {
                match action {
                    ClientAction::Connect => {
                        *session += 1;
                        host.connected(
                            *session,
                            Some(Caller {
                                name: "app".into(),
                                grants: IoGrants::default(),
                                definitions: None,
                            }),
                        );
                        to_client.extend(client.connected());
                    }
                    ClientAction::Send(frame) => {
                        let mut actions = vec![];
                        for action in host.frame(*session, frame, &mut Head, &Digest) {
                            match action {
                                Action::Arrive(arrival) => actions
                                    .extend(host.arrival_ended(arrival.id, store.end(&arrival))),
                                action => actions.push(action),
                            }
                        }
                        for action in actions {
                            match action {
                                Action::Send(_, Frame::Outcome(_)) if lose => {
                                    lose = false;
                                    host.disconnected(*session);
                                    to_client.extend(client.lost());
                                }
                                Action::Send(_, frame) => to_client.extend(client.frame(frame)),
                                _ => (),
                            }
                        }
                    }
                    ClientAction::Disconnect => {
                        host.disconnected(*session);
                    }
                    ClientAction::Complete(token, RunnerCompletion { outcome, .. }) => {
                        completed.push((token, outcome))
                    }
                }
            }
            from_client = to_client;
        }
    };
    let first = client.submit(1, call(1), [1; 32]);
    // The second transact waits for the first: one request in flight per stream.
    let second = client.submit(2, call(2), [2; 32]);
    assert!(second.is_empty(), "{second:?}");
    exchange(
        &mut client,
        &mut host,
        &mut store,
        &mut session,
        first,
        true,
    );
    assert_eq!(
        completed,
        [
            (1, CallOutcome::Ok(vec![11])),
            (2, CallOutcome::Ok(vec![12]))
        ]
    );
    assert_eq!(store.runs, [1, 2]);
    assert_eq!(session, 2, "the lost end needed a second session");
}
