//! Served calls at a `db@1` place (runners.md `db@1`): the host makes its share of the
//! RUN-8 checks, hands what passes to the head, and sends the end the head reports to the
//! stream's current session. The law: every served call gets exactly one end, on the
//! session that holds its stream when the end exists; the host never decides a served
//! position, so a reopened stream's `applied` does not move.
use hirsel_remote::frame::{Call, Frame, Outcome};
use hirsel_remote::host::{
    ABANDONED, Action, Arrival, Caller, DENIED, Digests, Host, Limits, OVERLOADED, Place, REJECTED,
    Refusal, UNREACHABLE,
};
use hirsel_remote::journal::NoJournal;
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
