//! The host and client protocols together under a seeded simulated network and executor:
//! connections that drop with frames in flight, frames duplicated or reordered within a
//! connection, frames of a closed connection delivered on its successor, cancellations,
//! timeouts, host crashes with and without a durable journal, and durable journal appends
//! that fail after writing none, part or all of their record (the host stops, and its
//! process restarts). Every schedule checks the protocol's promises:
//! - no call key's callee ever starts twice;
//! - every submitted call completes exactly once;
//! - `Ok` and `Failed` carry exactly what the callee produced, and a call completed
//!   `NotRun` or `Canceled` never started, then or later;
//! - with no faults, every call completes `Ok`;
//! - with a durable journal, every position is decided at most once, and every outcome the
//!   caller received is the one the recovered journal holds and its `Status` answers.
use hirsel_remote::client::{Client, ClientAction, Settings};
use hirsel_remote::frame::{Call, Frame};
use hirsel_remote::host::{Action, Caller, Digests, Host, Limits, Place, Refusal};
use hirsel_remote::journal::{Entry, Journal, NoJournal, SimDisk};
use hirsel_run_schema::{CallForm, CallOutcome, GrantSet, IoGrants, call_key};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// splitmix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn chance(&mut self, per_million: u64) -> bool {
        self.next() % 1_000_000 < per_million
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct TestDigests;
impl Digests for TestDigests {
    fn digest(&self, call: &Call) -> [u8; 32] {
        hirsel_run_schema::request_digest(
            call.form,
            &call.path,
            &call.fingerprint,
            &call.code,
            format!("{:?}", call.grants).as_bytes(),
            format!("{:?}", call.definitions).as_bytes(),
            &call.argument,
        )
    }
}
struct TestPlace;
impl Place for TestPlace {
    fn admit(&mut self, _: &Caller, call: &Call) -> Result<IoGrants, Refusal> {
        if call.path.ends_with("denied") {
            return Err(Refusal {
                reason: 0,
                missing: vec![],
            });
        }
        Ok(IoGrants::default())
    }
}

#[derive(Clone, Copy, Default)]
struct Faults {
    /// Per step, in parts per million.
    drop_connection: u64,
    duplicate: u64,
    reorder: u64,
    stale: u64,
    crash: u64,
    refuse_connect: u64,
    cancel: u64,
    /// Per journal append, in parts per million: the append fails.
    append: u64,
}

struct Connection {
    session: u64,
    to_host: VecDeque<Frame>,
    to_client: VecDeque<Frame>,
}

/// The callee executor: what started, and how each ended.
#[derive(Default)]
struct Executor {
    /// Running callees: run id -> (key, canceled, steps left).
    running: BTreeMap<u64, ([u8; 32], bool, u32)>,
    started: BTreeMap<[u8; 32], u32>,
    produced: BTreeMap<[u8; 32], CallOutcome>,
}

struct World {
    rng: Rng,
    faults: Faults,
    host: Host,
    disk: Option<SimDisk>,
    incarnations: u64,
    client: Client,
    connection: Option<Connection>,
    /// Frames of closed connections still in the network.
    ghosts: Vec<Frame>,
    next_session: u64,
    executor: Executor,
    completed: BTreeMap<u64, Vec<CallOutcome>>,
    calls: BTreeMap<u64, [u8; 32]>,
    /// Every decided outcome delivered to the caller, by position: its key and end.
    received: BTreeMap<u64, ([u8; 32], CallOutcome)>,
    now: u64,
}

fn caller() -> Caller {
    Caller {
        name: "app".into(),
        grants: IoGrants::default(),
        definitions: None,
    }
}
const INSTANCE: [u8; 16] = [7; 16];
/// `NotRunReason.Rejected()`: also the host's unrecorded answer to a stale, gap or
/// mismatched request.
const REJECTED: u32 = 4;

/// Start a host over `disk`, as its process does after a crash or a stop: a start whose
/// own appends fail exits, and the next one recovers again.
fn restart(disk: &SimDisk, fresh: [u8; 16], limits: Limits) -> Host {
    loop {
        let journal = disk.journal(fresh).expect("the journal recovers");
        if let Ok(host) = Host::new(Box::new(journal), limits) {
            return host;
        }
    }
}

impl World {
    fn new(seed: u64, faults: Faults, durable: bool, settings: Settings, limits: Limits) -> Self {
        let mut rng = Rng(seed);
        let disk = durable.then(SimDisk::default);
        let host = match &disk {
            Some(disk) => {
                // Most seeds compact the journal every few records, so crashes and
                // failures land on every step of a compaction too.
                if !seed.is_multiple_of(4) {
                    disk.compact_after(2 + seed % 9);
                }
                // Script the failing appends: none, part or all of the record is written.
                for attempt in 0..1_000 {
                    if faults.append > 0 && rng.chance(faults.append) {
                        let written = match rng.below(3) {
                            0 => 0,
                            1 => 1 + rng.below(80),
                            _ => usize::MAX,
                        };
                        disk.fail_append(attempt, written);
                    }
                }
                restart(disk, [1; 16], limits)
            }
            None => Host::new(Box::new(NoJournal::new([1; 16])), limits).unwrap(),
        };
        Self {
            rng,
            faults,
            host,
            disk,
            incarnations: 1,
            client: Client::new([3; 32], INSTANCE, settings),
            connection: None,
            ghosts: vec![],
            next_session: 1,
            executor: Executor::default(),
            completed: BTreeMap::new(),
            calls: BTreeMap::new(),
            received: BTreeMap::new(),
            now: 0,
        }
    }

    fn submit(&mut self, token: u64, path: &str) {
        let key = call_key(&INSTANCE, token);
        self.calls.insert(token, key);
        let call = Call {
            epoch: 0,
            position: 0,
            ordinal: token,
            key,
            form: CallForm::Call,
            path: path.into(),
            fingerprint: [1; 32],
            code: [2; 32],
            grants: GrantSet::default(),
            definitions: None,
            argument: token.to_le_bytes().to_vec(),
            acknowledged: 0,
            level: None,
        };
        let digest = TestDigests.digest(&call);
        let actions = self.client.submit(token, call, digest);
        self.client_actions(actions);
    }

    fn client_actions(&mut self, actions: Vec<ClientAction>) {
        for action in actions {
            match action {
                ClientAction::Connect => {
                    if self.rng.chance(self.faults.refuse_connect) {
                        let actions = self.client.lost();
                        self.client_actions(actions);
                        continue;
                    }
                    self.close_connection();
                    let session = self.next_session;
                    self.next_session += 1;
                    self.connection = Some(Connection {
                        session,
                        to_host: VecDeque::new(),
                        to_client: VecDeque::new(),
                    });
                    self.host.connected(session, Some(caller()));
                    let actions = self.client.connected();
                    self.client_actions(actions);
                }
                ClientAction::Send(frame) => {
                    if let Some(c) = &mut self.connection {
                        c.to_host.push_back(frame);
                    }
                }
                ClientAction::Disconnect => {
                    if let Some(c) = self.connection.take() {
                        self.ghosts.extend(c.to_host);
                        let actions = self.host.disconnected(c.session);
                        self.host_actions(actions);
                    }
                }
                ClientAction::Complete(token, completion) => {
                    self.completed
                        .entry(token)
                        .or_default()
                        .push(completion.outcome);
                }
            }
        }
    }

    /// The connection breaks: both ends learn of it, and what it carried is lost.
    fn close_connection(&mut self) {
        if let Some(c) = self.connection.take() {
            self.ghosts.extend(c.to_host);
            let actions = self.host.disconnected(c.session);
            self.host_actions(actions);
            let actions = self.client.lost();
            self.client_actions(actions);
        }
    }

    fn host_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(session, frame) => {
                    if let Some(c) = &mut self.connection
                        && c.session == session
                    {
                        c.to_client.push_back(frame);
                    }
                }
                Action::Close(session) => {
                    if self
                        .connection
                        .as_ref()
                        .is_some_and(|c| c.session == session)
                    {
                        // The host closes after its last frames reach the wire.
                        let c = self.connection.as_mut().unwrap();
                        let pending: Vec<Frame> = c.to_client.drain(..).collect();
                        for frame in pending {
                            self.deliver(frame);
                        }
                        if let Some(c) = self.connection.take() {
                            self.ghosts.extend(c.to_host);
                            let actions = self.host.disconnected(c.session);
                            self.host_actions(actions);
                        }
                        let actions = self.client.lost();
                        self.client_actions(actions);
                    }
                }
                Action::Start(start) => {
                    *self.executor.started.entry(start.call.key).or_default() += 1;
                    let steps = 1 + self.rng.below(6) as u32;
                    self.executor
                        .running
                        .insert(start.run, (start.call.key, false, steps));
                }
                Action::Cancel(run, _) => {
                    if let Some(r) = self.executor.running.get_mut(&run) {
                        r.1 = true;
                    }
                }
                Action::Arrive(arrival) => panic!("the simulated host is no place: {arrival:?}"),
            }
        }
    }

    /// A frame reaches the caller.
    fn deliver(&mut self, frame: Frame) {
        // Positions restart with a host that forgets its streams.
        if let Frame::Outcome(o) = &frame
            && self.disk.is_some()
            && o.end != CallOutcome::NotRun(REJECTED)
        {
            let seen = (o.key, o.end.clone());
            let earlier = self.received.insert(o.position, seen.clone());
            assert!(
                earlier.as_ref().is_none_or(|e| *e == seen),
                "position {} received {earlier:?} and then {seen:?}",
                o.position
            );
        }
        let actions = self.client.frame(frame);
        self.client_actions(actions);
    }

    fn crash_host(&mut self) {
        let connected = self.connection.take().is_some();
        self.ghosts.clear();
        // Every running callee stops where it is: it may have done part of its work.
        self.executor.running.clear();
        self.incarnations += 1;
        let fresh = [self.incarnations as u8; 16];
        self.host = match &self.disk {
            Some(disk) => restart(disk, fresh, Limits::default()),
            None => Host::new(Box::new(NoJournal::new(fresh)), Limits::default()).unwrap(),
        };
        // The client sees its connection drop and reconnects to the restarted host.
        if connected {
            let actions = self.client.lost();
            self.client_actions(actions);
        }
    }

    /// One scheduling step; false when nothing is left to do.
    fn step(&mut self) -> bool {
        self.now += 1_000_000;
        let actions = self.client.tick(self.now);
        self.client_actions(actions);
        let f = self.faults;
        // A host stopped by a failed append exits; its process restarts and recovers.
        if self.host.failure().is_some() {
            self.crash_host();
            return true;
        }
        if self.connection.is_some() && self.rng.chance(f.drop_connection) {
            self.close_connection();
            return true;
        }
        if self.rng.chance(f.crash) {
            self.crash_host();
            return true;
        }
        if !self.calls.is_empty() && self.rng.chance(f.cancel) {
            let tokens: Vec<u64> = self.calls.keys().copied().collect();
            let token = tokens[self.rng.below(tokens.len())];
            let actions = self.client.cancel(token, 0);
            self.client_actions(actions);
        }
        let mut choices = vec![];
        if let Some(c) = &self.connection {
            if !c.to_host.is_empty() {
                choices.push(0);
            }
            if !c.to_client.is_empty() {
                choices.push(1);
            }
        }
        if !self.executor.running.is_empty() {
            choices.push(2);
        }
        if !self.ghosts.is_empty() && self.connection.is_some() {
            choices.push(3);
        }
        if choices.is_empty() {
            return self.client.busy();
        }
        match choices[self.rng.below(choices.len())] {
            0 => {
                let c = self.connection.as_mut().unwrap();
                if c.to_host.len() > 1 && self.rng.chance(f.reorder) {
                    c.to_host.swap(0, 1);
                }
                let frame = c.to_host.pop_front().unwrap();
                if matches!(frame, Frame::Call(_) | Frame::Cancel(..) | Frame::Status(_))
                    && self.rng.chance(f.duplicate)
                {
                    c.to_host.push_front(frame.clone());
                }
                let session = c.session;
                let actions = self
                    .host
                    .frame(session, frame, &mut TestPlace, &TestDigests);
                self.host_actions(actions);
            }
            1 => {
                let c = self.connection.as_mut().unwrap();
                if c.to_client.len() > 1 && self.rng.chance(f.reorder) {
                    c.to_client.swap(0, 1);
                }
                let frame = c.to_client.pop_front().unwrap();
                if self.rng.chance(f.duplicate) {
                    c.to_client.push_front(frame.clone());
                }
                self.deliver(frame);
            }
            2 => {
                let runs: Vec<u64> = self.executor.running.keys().copied().collect();
                let run = runs[self.rng.below(runs.len())];
                let entry = self.executor.running.get_mut(&run).unwrap();
                entry.2 -= 1;
                if entry.2 == 0 || entry.1 {
                    let (key, canceled, _) = self.executor.running.remove(&run).unwrap();
                    let end = if canceled {
                        CallOutcome::Failed(b"canceled".to_vec())
                    } else {
                        CallOutcome::Ok(key[..4].to_vec())
                    };
                    self.executor.produced.insert(key, end.clone());
                    let actions = self.host.ended(run, end, Some([9; 16]));
                    self.host_actions(actions);
                }
            }
            _ => {
                // A frame of a closed connection arrives on the open one.
                let i = self.rng.below(self.ghosts.len());
                let frame = self.ghosts.remove(i);
                if self.rng.chance(f.stale) {
                    let session = self.connection.as_ref().unwrap().session;
                    let actions = self
                        .host
                        .frame(session, frame, &mut TestPlace, &TestDigests);
                    self.host_actions(actions);
                }
            }
        }
        true
    }

    fn check(&self, faults: bool) {
        for (key, count) in &self.executor.started {
            assert_eq!(*count, 1, "a callee started {count} times");
            let _ = key;
        }
        for (token, key) in &self.calls {
            let outcomes = &self.completed[token];
            assert_eq!(outcomes.len(), 1, "call {token} completed {outcomes:?}");
            match &outcomes[0] {
                CallOutcome::Ok(bytes) | CallOutcome::Failed(bytes) => {
                    assert_eq!(
                        self.executor.produced.get(key),
                        Some(&outcomes[0]),
                        "call {token}: {bytes:?} is not what its callee produced"
                    );
                }
                CallOutcome::NotRun(_) | CallOutcome::Canceled(_) => {
                    assert!(
                        !self.executor.started.contains_key(key),
                        "call {token} completed {:?} but its callee started",
                        outcomes[0]
                    );
                }
                CallOutcome::InDoubt(_) => {}
                CallOutcome::Faulted(_) => panic!("no callee faults here"),
            }
            if !faults && !self.calls_denied().contains(token) {
                assert!(
                    matches!(outcomes[0], CallOutcome::Ok(_)),
                    "call {token} ended {:?} without faults",
                    outcomes[0]
                );
            }
        }
    }
    /// The recovered journal decides each position at most once, and holds every outcome
    /// the caller received; a host restarted over it answers `Status` with that outcome for
    /// every position the caller has not acknowledged.
    fn check_recovery(&self) {
        let Some(disk) = &self.disk else {
            return;
        };
        check_recovery(disk, Limits::default(), [3; 32], &self.received);
    }

    fn calls_denied(&self) -> BTreeSet<u64> {
        self.calls.keys().copied().filter(|t| t % 7 == 6).collect()
    }
}

/// The same durable-outcome oracle for the client world and adversarial streams.
fn check_recovery(
    disk: &SimDisk,
    limits: Limits,
    stream: [u8; 32],
    received: &BTreeMap<u64, ([u8; 32], CallOutcome)>,
) {
    let mut decided = BTreeMap::new();
    let mut acknowledged = 0;
    for entry in disk.journal([0; 16]).unwrap().recover().unwrap() {
        match entry {
            Entry::Decided {
                stream: owner,
                position,
                key,
                end,
                ..
            } if owner.stream == stream => {
                let earlier = decided.insert(position, (key, end));
                assert!(earlier.is_none(), "position {position} decided twice");
            }
            Entry::Acknowledged {
                stream: owner,
                position,
            } if owner.stream == stream => acknowledged = position,
            _ => (),
        }
    }
    let mut host = restart(disk, [0; 16], limits);
    host.connected(1, Some(caller()));
    let open = Frame::Open {
        stream,
        instance: INSTANCE,
        epoch: u64::MAX,
    };
    host.frame(1, open, &mut TestPlace, &TestDigests);
    for (position, (key, end)) in received {
        // A compaction keeps no decision the caller acknowledged.
        match decided.get(position) {
            None => assert!(
                *position <= acknowledged,
                "position {position}: the caller received {end:?}, and it is lost"
            ),
            recovered => assert_eq!(
                recovered,
                Some(&(*key, end.clone())),
                "position {position}: the caller received {end:?}"
            ),
        }
        let status = Frame::Status(hirsel_remote::frame::CallRef {
            epoch: u64::MAX,
            position: *position,
            key: *key,
        });
        match &host.frame(1, status, &mut TestPlace, &TestDigests)[..] {
            // The caller acknowledged it.
            [] => (),
            [Action::Send(1, Frame::Outcome(o))] => assert_eq!(&o.end, end),
            other => panic!("position {position}: status after recovery is {other:?}"),
        }
    }
}

fn run(seed: u64, faults: Faults, durable: bool, settings: Settings) -> World {
    let mut world = World::new(
        seed,
        faults,
        durable,
        settings,
        Limits {
            runs: 2,
            queue: 64,
            ..Limits::default()
        },
    );
    let calls = 12;
    let mut submitted = 0;
    for _ in 0..20_000 {
        if submitted < calls && world.rng.chance(200_000) {
            let path = if submitted % 7 == 6 {
                "app@1/m.denied"
            } else {
                "app@1/m.charge"
            };
            world.submit(submitted, path);
            submitted += 1;
            continue;
        }
        if !world.step() && submitted == calls {
            break;
        }
    }
    assert_eq!(submitted, calls, "seed {seed}: every call was submitted");
    assert!(!world.client.busy(), "seed {seed}: calls left unfinished");
    world
}

#[test]
fn without_faults_every_call_runs_once_and_completes_ok() {
    for seed in 0..50 {
        let world = run(
            seed,
            Faults::default(),
            false,
            Settings {
                reconnect: 1,
                ..Settings::default()
            },
        );
        world.check(false);
        // Denied calls completed NotRun(Denied()).
        for token in world.calls_denied() {
            assert_eq!(world.completed[&token], vec![CallOutcome::NotRun(0)]);
        }
    }
}

fn faulty(seed: u64, durable: bool, crash: u64, append: u64) {
    let faults = Faults {
        drop_connection: 20_000,
        duplicate: 100_000,
        reorder: 100_000,
        stale: 500_000,
        crash,
        refuse_connect: 100_000,
        cancel: 5_000,
        append,
    };
    let settings = Settings {
        timeout_ns: Some(2_000_000_000),
        reconnect: 3,
        retry: 2,
        reconcile: durable,
    };
    let world = run(seed, faults, durable, settings);
    world.check(true);
    world.check_recovery();
}

#[test]
fn lost_duplicated_and_reordered_frames_never_run_a_call_twice() {
    for seed in 0..300 {
        faulty(seed, false, 0, 0);
    }
}

#[test]
fn host_crashes_without_a_journal_leave_sent_calls_in_doubt() {
    for seed in 0..300 {
        faulty(seed, false, 3_000, 0);
    }
}

#[test]
fn host_crashes_with_a_durable_journal_resolve_calls_from_it() {
    for seed in 0..300 {
        faulty(seed, true, 3_000, 0);
    }
}

/// A failed append stops the host and its process restarts, so no outcome is sent for a
/// decision the journal does not hold, and no callee starts for an admission it lost.
#[test]
fn failed_journal_appends_stop_the_host_and_recovery_agrees_with_every_outcome_sent() {
    for seed in 0..300 {
        faulty(seed, true, 3_000, 30_000);
    }
}

/// A host that recovers a durable journal answers a status query for a call it admitted
/// and never finished with `InDoubt(Abandoned())`, and for a call key it never saw, at a
/// position above its applied one, the client concludes `NotRun(Unreachable())`.
#[test]
fn a_crash_mid_call_is_abandoned_and_an_undelivered_call_never_ran() {
    let mut world = World::new(
        1,
        Faults::default(),
        true,
        Settings {
            reconnect: 2,
            reconcile: true,
            ..Settings::default()
        },
        Limits {
            runs: 1,
            queue: 8,
            ..Limits::default()
        },
    );
    world.submit(0, "app@1/m.charge");
    // Deliver Open, Opened and the Call: the callee starts.
    while world.executor.started.is_empty() {
        assert!(world.step());
        world
            .executor
            .running
            .values_mut()
            .for_each(|r| r.2 = u32::MAX);
    }
    // A second call is sent but never reaches the host.
    world.submit(1, "app@1/m.charge");
    world.connection.as_mut().unwrap().to_host.clear();
    world.crash_host();
    for _ in 0..1000 {
        if !world.step() {
            break;
        }
    }
    assert_eq!(world.completed[&0], vec![CallOutcome::InDoubt(2)]);
    // The undelivered call was proved never to have run and was re-sent once... it has no
    // retries, so it ends NotRun(Unreachable()).
    assert_eq!(world.completed[&1], vec![CallOutcome::NotRun(2)]);
    world.check(true);
}

/// Duplicates of a decided call get its stored outcome and never run it again; a call at
/// a position beyond the next is refused, and stays refused when the gap closes.
#[test]
fn duplicates_get_the_stored_outcome_and_gaps_stay_refused() {
    let mut host = Host::new(Box::new(NoJournal::new([1; 16])), Limits::default()).unwrap();
    host.connected(1, Some(caller()));
    let open = Frame::Open {
        stream: [3; 32],
        instance: INSTANCE,
        epoch: 1,
    };
    let actions = host.frame(1, open, &mut TestPlace, &TestDigests);
    assert!(matches!(
        actions[..],
        [Action::Send(1, Frame::Opened { applied: 0, .. })]
    ));
    let call = |position: u64, ordinal: u64| Call {
        epoch: 1,
        position,
        ordinal,
        key: call_key(&INSTANCE, ordinal),
        form: CallForm::Compute,
        path: "app@1/m.f".into(),
        fingerprint: [0; 32],
        code: [0; 32],
        grants: GrantSet::default(),
        definitions: None,
        argument: vec![ordinal as u8],
        acknowledged: 0,
        level: None,
    };
    let send = |host: &mut Host, c: Call| {
        host.frame(1, Frame::Call(Box::new(c)), &mut TestPlace, &TestDigests)
    };
    // Position 2 before 1: a gap.
    let gap = send(&mut host, call(2, 1));
    let [Action::Send(1, Frame::Outcome(o))] = &gap[..] else {
        panic!("{gap:?}")
    };
    assert_eq!((o.position, &o.end), (2, &CallOutcome::NotRun(4)));
    // Position 1 runs; the gap at 2 is decided refused in the same step.
    let first = send(&mut host, call(1, 0));
    let started: Vec<_> = first
        .iter()
        .filter_map(|a| match a {
            Action::Start(s) => Some(s.run),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1);
    // The refused position stays refused for whatever arrives there.
    let again = send(&mut host, call(2, 1));
    assert!(
        again.is_empty()
            || matches!(&again[..], [Action::Send(_, Frame::Outcome(o))] if o.end == CallOutcome::NotRun(4))
    );
    // A duplicate of the running call waits for its outcome; nothing starts.
    assert!(send(&mut host, call(1, 0)).is_empty());
    let ended = host.ended(started[0], CallOutcome::Ok(vec![42]), None);
    assert!(
        matches!(&ended[..], [Action::Send(1, Frame::Outcome(o))] if o.end == CallOutcome::Ok(vec![42]))
    );
    // A duplicate after the end gets the stored outcome.
    let dup = send(&mut host, call(1, 0));
    assert!(
        matches!(&dup[..], [Action::Send(1, Frame::Outcome(o))] if o.end == CallOutcome::Ok(vec![42]))
    );
    // The same position with another request is rejected, and the stored outcome stands.
    let mut other = call(1, 0);
    other.argument = vec![99];
    let rejected = send(&mut host, other);
    assert!(
        matches!(&rejected[..], [Action::Send(1, Frame::Outcome(o))] if o.end == CallOutcome::NotRun(4))
    );
    assert!(!host.busy());
}

/// A frame of a fenced session is discarded, and a stale epoch cannot reopen the stream.
#[test]
fn a_fenced_session_is_discarded() {
    let mut host = Host::new(Box::new(NoJournal::new([1; 16])), Limits::default()).unwrap();
    let open = |epoch| Frame::Open {
        stream: [3; 32],
        instance: INSTANCE,
        epoch,
    };
    host.connected(1, Some(caller()));
    host.frame(1, open(1), &mut TestPlace, &TestDigests);
    host.connected(2, Some(caller()));
    let actions = host.frame(2, open(2), &mut TestPlace, &TestDigests);
    assert!(actions.contains(&Action::Close(1)));
    // Session 1 is gone; an epoch-1 frame on session 2 is discarded.
    let status = Frame::Status(hirsel_remote::frame::CallRef {
        epoch: 1,
        position: 1,
        key: [0; 32],
    });
    assert!(
        host.frame(2, status, &mut TestPlace, &TestDigests)
            .is_empty()
    );
    host.connected(3, Some(caller()));
    let stale = host.frame(3, open(2), &mut TestPlace, &TestDigests);
    assert_eq!(
        stale,
        vec![Action::Send(3, Frame::Refused(0)), Action::Close(3)]
    );
    // An identity the host does not admit is refused.
    host.connected(4, None);
    let refused = host.frame(4, open(9), &mut TestPlace, &TestDigests);
    assert_eq!(
        refused,
        vec![Action::Send(4, Frame::Refused(0)), Action::Close(4)]
    );
}

fn open(epoch: u64) -> Frame {
    Frame::Open {
        stream: [3; 32],
        instance: INSTANCE,
        epoch,
    }
}
fn call(epoch: u64, position: u64) -> Call {
    Call {
        epoch,
        position,
        ordinal: position,
        key: call_key(&INSTANCE, position),
        form: CallForm::Compute,
        path: "app@1/m.f".into(),
        fingerprint: [0; 32],
        code: [0; 32],
        grants: GrantSet::default(),
        definitions: None,
        argument: vec![position as u8],
        acknowledged: 0,
        level: None,
    }
}
fn status(epoch: u64, position: u64) -> Frame {
    Frame::Status(hirsel_remote::frame::CallRef {
        epoch,
        position,
        key: call_key(&INSTANCE, position),
    })
}
/// Send `c`, which starts its callee, and end the callee with `Ok([end])`.
fn run_to_end(host: &mut Host, session: u64, c: Call, end: u8) -> Vec<Action> {
    let actions = host.frame(
        session,
        Frame::Call(Box::new(c)),
        &mut TestPlace,
        &TestDigests,
    );
    let [Action::Start(start)] = &actions[..] else {
        panic!("{actions:?}")
    };
    host.ended(start.run, CallOutcome::Ok(vec![end]), None)
}

/// An append that writes part of its record stops the host, so no later record follows the
/// torn one; the restarted host cuts it, and the next record it appends survives recovery.
#[test]
fn a_partial_append_stops_the_host_and_the_next_record_survives_restart() {
    let disk = SimDisk::default();
    let mut host = restart(&disk, [1; 16], Limits::default());
    host.connected(1, Some(caller()));
    host.frame(1, open(1), &mut TestPlace, &TestDigests);
    let ended = run_to_end(&mut host, 1, call(1, 1), 41);
    assert!(
        matches!(&ended[..], [Action::Send(1, Frame::Outcome(o))] if o.end == CallOutcome::Ok(vec![41]))
    );
    // Position 2's admission writes 20 bytes of its record and fails: nothing starts, the
    // session closes, and the host refuses everything after.
    let intact = disk.bytes();
    disk.fail_append(disk.appends(), 20);
    let actions = host.frame(
        1,
        Frame::Call(Box::new(call(1, 2))),
        &mut TestPlace,
        &TestDigests,
    );
    assert_eq!(actions, vec![Action::Close(1)]);
    assert!(host.failure().is_some());
    host.connected(2, Some(caller()));
    assert_eq!(
        host.frame(2, open(2), &mut TestPlace, &TestDigests),
        vec![Action::Close(2)]
    );
    assert_eq!(
        disk.bytes().len(),
        intact.len() + 20,
        "nothing follows the torn record"
    );
    // The restart cuts the torn record: position 1 stands, position 2 was never decided.
    let mut host = restart(&disk, [2; 16], Limits::default());
    assert_eq!(disk.bytes(), intact);
    host.connected(3, Some(caller()));
    let opened = host.frame(3, open(3), &mut TestPlace, &TestDigests);
    assert!(matches!(
        opened[..],
        [Action::Send(3, Frame::Opened { applied: 1, .. })]
    ));
    run_to_end(&mut host, 3, call(3, 2), 42);
    // Another restart recovers the record appended after the cut.
    let mut host = restart(&disk, [3; 16], Limits::default());
    host.connected(4, Some(caller()));
    host.frame(4, open(4), &mut TestPlace, &TestDigests);
    for (position, end) in [(1, 41), (2, 42)] {
        let answer = host.frame(4, status(4, position), &mut TestPlace, &TestDigests);
        assert!(
            matches!(&answer[..], [Action::Send(4, Frame::Outcome(o))] if o.end == CallOutcome::Ok(vec![end])),
            "position {position}: {answer:?}"
        );
    }
}

/// A checkpoint drops the outcomes the caller acknowledged and keeps their positions: a
/// host recovered from it answers `Opened` with the same applied position, and runs the
/// next position rather than refusing it as a gap.
#[test]
fn a_compacted_journal_keeps_the_applied_position_of_acknowledged_outcomes() {
    let disk = SimDisk::default();
    disk.compact_after(2);
    let mut host = restart(&disk, [1; 16], Limits::default());
    // Run and acknowledge one call per session until a checkpoint holds the stream's
    // acknowledgement and none of its decisions.
    let mut position = 0;
    loop {
        position += 1;
        assert!(
            position < 20,
            "no checkpoint dropped the acknowledged decisions"
        );
        host.connected(position, Some(caller()));
        host.frame(position, open(position), &mut TestPlace, &TestDigests);
        run_to_end(&mut host, position, call(position, position), 40);
        let close = Frame::Close {
            epoch: position,
            acknowledged: position,
        };
        host.frame(position, close, &mut TestPlace, &TestDigests);
        let records = disk.journal([0; 16]).unwrap().recover().unwrap();
        if !records.iter().any(|e| matches!(e, Entry::Decided { .. })) {
            break;
        }
    }
    let mut host = restart(&disk, [2; 16], Limits::default());
    host.connected(99, Some(caller()));
    let opened = host.frame(99, open(99), &mut TestPlace, &TestDigests);
    assert!(
        matches!(&opened[..], [Action::Send(99, Frame::Opened { applied, .. })] if *applied == position),
        "{opened:?}"
    );
    run_to_end(&mut host, 99, call(99, position + 1), 41);
}

/// Withholding acknowledgements, reordered gaps, changed requests, session refusal,
/// acknowledgement and honest sequential calls share this law. Every history crosses
/// compaction and recovery, preserving received outcomes and at-most-once execution.
#[test]
fn an_adversarial_caller_keeps_host_state_and_frame_work_within_its_limits() {
    let limits = Limits {
        runs: 1,
        queue: 2,
        streams: 4,
        unacknowledged: 3,
    };
    let window = (limits.runs + limits.queue) as usize;
    let check = |seed: u64, host: &Host, actions: &[Action]| {
        assert!(actions.len() <= window + 1, "seed {seed}: {actions:?}");
        let held = host.retained();
        assert!(held.gaps < window, "seed {seed}: {held:?}");
        // Only the main stream receives calls; its retention bound is the window.
        assert!(held.decided <= window, "seed {seed}: {held:?}");
        assert!(held.decided as u64 <= 1 + seed % 3, "seed {seed}: {held:?}");
        assert!(
            held.streams <= limits.streams as usize,
            "seed {seed}: {held:?}"
        );
    };
    let remember = |received: &mut BTreeMap<u64, ([u8; 32], CallOutcome)>,
                    actions: &[Action],
                    applied: u64| {
        for action in actions {
            if let Action::Send(_, Frame::Outcome(o)) = action
                && o.position <= applied
            {
                let seen = (o.key, o.end.clone());
                let old = received.insert(o.position, seen.clone());
                assert!(
                    old.as_ref().is_none_or(|o| *o == seen),
                    "position {} received {old:?} then {seen:?}",
                    o.position
                );
            }
        }
    };
    let request = |epoch, position, ordinal, acknowledged, argument| Call {
        epoch,
        position,
        ordinal,
        key: call_key(&INSTANCE, ordinal),
        form: CallForm::Compute,
        path: "app@1/m.f".into(),
        fingerprint: [0; 32],
        code: [0; 32],
        grants: GrantSet::default(),
        definitions: None,
        argument: vec![argument],
        acknowledged,
        level: None,
    };
    let mut refusals = 0;
    let mut resumed = 0;
    let mut recovered_full = 0;
    let mut gap_refusals = 0;
    for honest in [false, true] {
        for seed in 0..50 {
            let limits = Limits {
                unacknowledged: 1 + seed % 3,
                ..limits
            };
            let disk = SimDisk::default();
            disk.compact_after(2 + seed % 9);
            let mut rng = Rng(seed);
            let mut host = restart(&disk, [1; 16], limits);
            let mut started = BTreeSet::new();
            let mut running = vec![];
            let mut received = BTreeMap::new();
            let mut session = 0;
            let mut epoch = 0;
            let mut applied = 0;
            let mut opened = 0;
            let mut connected = false;
            let mut compacted = false;
            let mut refused: BTreeMap<u64, CallOutcome> = BTreeMap::new();
            for step in 0..4_000u64 {
                if !connected || step % 500 == 0 {
                    session += 1;
                    epoch += 1;
                    host.connected(session, Some(caller()));
                    let open = Frame::Open {
                        stream: [0; 32],
                        instance: INSTANCE,
                        epoch,
                    };
                    let actions = host.frame(session, open, &mut TestPlace, &TestDigests);
                    check(seed, &host, &actions);
                    let [.., Action::Send(_, Frame::Opened { applied: a, .. })] = actions[..]
                    else {
                        panic!("seed {seed}: {actions:?}")
                    };
                    assert_eq!(a, applied, "seed {seed}: recovery moved applied");
                    connected = true;
                    refused.clear();
                    // Recovery remains available even when no new position can fit.
                    let full = host.retained().decided == limits.unacknowledged as usize;
                    let records = disk.journal([0; 16]).unwrap().recover().unwrap();
                    for entry in records {
                        if let Entry::Decided {
                            stream,
                            position,
                            key,
                            ..
                        } = entry
                            && stream.stream == [0; 32]
                        {
                            let status = Frame::Status(hirsel_remote::frame::CallRef {
                                epoch,
                                position,
                                key,
                            });
                            let actions = host.frame(session, status, &mut TestPlace, &TestDigests);
                            check(seed, &host, &actions);
                            remember(&mut received, &actions, applied);
                            if full {
                                // Even a mismatched duplicate gets its position-level
                                // response, rather than a retention session refusal.
                                let mut duplicate = request(epoch, position, position, 0, 0);
                                duplicate.key = key;
                                let actions = host.frame(
                                    session,
                                    Frame::Call(Box::new(duplicate)),
                                    &mut TestPlace,
                                    &TestDigests,
                                );
                                check(seed, &host, &actions);
                                assert!(matches!(
                                    &actions[..],
                                    [Action::Send(_, Frame::Outcome(_))]
                                ));
                            }
                            if full
                                && actions
                                    .iter()
                                    .any(|a| matches!(a, Action::Send(_, Frame::Outcome(_))))
                            {
                                recovered_full += 1;
                            }
                        }
                    }
                    if step % 500 == 0 {
                        // Six more streams, of which the caller may hold four in all.
                        for _ in 0..6 {
                            opened += 1;
                            let other = 1_000_000 + opened;
                            host.connected(other, Some(caller()));
                            let open = Frame::Open {
                                stream: [opened as u8; 32],
                                instance: INSTANCE,
                                epoch: 1,
                            };
                            let actions = host.frame(other, open, &mut TestPlace, &TestDigests);
                            check(seed, &host, &actions);
                            let expected = if opened < limits.streams {
                                matches!(actions[..], [Action::Send(_, Frame::Opened { .. })])
                            } else {
                                actions
                                    == vec![
                                        Action::Send(other, Frame::Refused(3)),
                                        Action::Close(other),
                                    ]
                            };
                            assert!(expected, "seed {seed}: stream {opened}: {actions:?}");
                        }
                    }
                }
                let before = disk.appends();
                let full = host.retained().decided == limits.unacknowledged as usize;
                let mut unacknowledging = false;
                let mut acknowledging = false;
                let actions = if !running.is_empty() && (honest || rng.chance(300_000)) {
                    let run = running.remove(rng.below(running.len()));
                    host.ended(run, CallOutcome::Ok(vec![1]), None)
                } else if rng.chance(20_000) {
                    connected = false;
                    host.frame(
                        session,
                        Frame::Close {
                            epoch,
                            acknowledged: applied,
                        },
                        &mut TestPlace,
                        &TestDigests,
                    )
                } else {
                    let position = if honest {
                        applied + 1
                    } else {
                        match rng.below(4) {
                            0 => applied + 1,
                            1 => applied + 1 + rng.below(window + 2) as u64,
                            _ => applied + 1 + rng.below(200_000) as u64,
                        }
                    };
                    let ordinal = if honest {
                        position
                    } else {
                        rng.below(64) as u64
                    };
                    let acknowledged = if honest || rng.chance(100_000) {
                        applied
                    } else {
                        0
                    };
                    if honest && applied > 0 {
                        assert!(
                            received.contains_key(&applied),
                            "honest caller acknowledges a received outcome"
                        );
                    }
                    unacknowledging = acknowledged == 0;
                    acknowledging = acknowledged == applied && position == applied + 1;
                    let call = request(epoch, position, ordinal, acknowledged, rng.below(2) as u8);
                    host.frame(
                        session,
                        Frame::Call(Box::new(call)),
                        &mut TestPlace,
                        &TestDigests,
                    )
                };
                check(seed, &host, &actions);
                let rejected_session = actions
                    .iter()
                    .any(|a| matches!(a, Action::Send(_, Frame::Refused(3))));
                assert!(
                    !honest || !rejected_session,
                    "seed {seed}: honest caller refused: {actions:?}"
                );
                if full && unacknowledging {
                    assert_eq!(
                        actions,
                        vec![
                            Action::Send(session, Frame::Refused(3)),
                            Action::Close(session)
                        ]
                    );
                    assert_eq!(disk.appends(), before, "refusal must not journal anything");
                    let late = Frame::Call(Box::new(request(
                        epoch,
                        applied + 1,
                        applied + 1,
                        applied,
                        0,
                    )));
                    assert!(
                        host.frame(session, late, &mut TestPlace, &TestDigests)
                            .is_empty(),
                        "closed session accepted a frame"
                    );
                    assert_eq!(disk.appends(), before);
                }
                let records = disk.journal([0; 16]).unwrap().recover().unwrap();
                let mut after = 0;
                let mut acknowledged = 0;
                let mut recorded = BTreeSet::new();
                for entry in records {
                    match entry {
                        Entry::Decided {
                            stream, position, ..
                        } if stream.stream == [0; 32] => {
                            after = after.max(position);
                            recorded.insert(position);
                        }
                        Entry::Admitted {
                            stream, position, ..
                        } if stream.stream == [0; 32] => after = after.max(position),
                        Entry::Acknowledged { stream, position } if stream.stream == [0; 32] => {
                            acknowledged = position;
                            after = after.max(position);
                        }
                        _ => (),
                    }
                }
                if full && unacknowledging {
                    assert_eq!(after, applied, "refusal moved applied");
                }
                if full && acknowledging && after > applied {
                    resumed += 1;
                }
                if rejected_session && !full && after > applied {
                    gap_refusals += 1;
                }
                remember(&mut received, &actions, after);
                compacted |= received
                    .keys()
                    .any(|p| *p <= acknowledged && !recorded.contains(p));
                for action in &actions {
                    match action {
                        Action::Start(start) => {
                            assert!(
                                started.insert(start.call.position),
                                "seed {seed}: position {} ran twice",
                                start.call.position
                            );
                            running.push(start.run);
                        }
                        Action::Send(_, Frame::Outcome(o)) => {
                            if o.position > applied && o.position <= after {
                                if let Some(earlier) = refused.remove(&o.position) {
                                    assert_eq!(
                                        o.end, earlier,
                                        "seed {seed}: position {} changed refusal",
                                        o.position
                                    );
                                }
                            } else if o.position > after && matches!(o.end, CallOutcome::NotRun(_))
                            {
                                refused.entry(o.position).or_insert(o.end.clone());
                            }
                        }
                        _ => (),
                    }
                }
                applied = after;
                if rejected_session {
                    refusals += 1;
                    connected = false;
                    let actions = host.disconnected(session);
                    check(seed, &host, &actions);
                    refused.clear();
                }
                if step % 137 == 136 {
                    // A real crash ends running callees; recovery records Abandoned.
                    drop(host);
                    running.clear();
                    check_recovery(&disk, limits, [0; 32], &received);
                    host = restart(&disk, [2; 16], limits);
                    check(seed, &host, &[]);
                    connected = false;
                    refused.clear();
                }
            }
            drop(host);
            check_recovery(&disk, limits, [0; 32], &received);
            assert!(
                compacted,
                "seed {seed}, honest={honest}: no acknowledged decision left the checkpoint"
            );
        }
    }
    assert!(
        refusals > 0 && resumed > 0 && recovered_full > 0 && gap_refusals > 0,
        "refusals={refusals}, resumed={resumed}, recovered_full={recovered_full}, gap_refusals={gap_refusals}"
    );
}
