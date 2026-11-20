//! Deterministic network and clock, shared by evaluator and native providers.
//! Adapted from FoundationDB sim2.cpp's scheduled delivery and simulated time.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

mod tls;
pub use tls::{CertFailure, TlsFailure, TlsHandshake, TlsIdentity, TlsScript, TlsStep};
use tls::{RECORD_BYTES, ScriptedPeer};

// Host registry generations start at one. Model identities use generation zero.
const LAST_ID: u64 = u32::MAX as u64;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub family: u8,
    pub hi: u64,
    pub lo: u64,
    pub port: u16,
}
#[derive(Clone, Debug)]
pub struct Config {
    /// Separate bound for each direction's accepted, transit and readable queue.
    pub queue_bytes: usize,
    pub max_prefix: usize,
    pub max_handles: usize,
    pub latency_ns: u64,
    pub seed: u64,
    /// Zero-based choices. Unspecified suffix choices come from the seed.
    pub prefix: BTreeMap<Choice, Vec<usize>>,
    /// Keep the schedule for prefix exploration. Long laws may omit the trace.
    pub record_decisions: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            queue_bytes: 8,
            max_prefix: 8,
            max_handles: 256,
            latency_ns: 0,
            seed: 1,
            prefix: BTreeMap::new(),
            record_decisions: true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Choice {
    Send,
    Receive,
    Delivery,
    Fault,
    Schedule,
}
impl Choice {
    const COUNT: usize = 5;
}

/// The model and its scripted dispatcher use the same deterministic generator.
pub struct SplitMix64(u64);
impl SplitMix64 {
    pub fn new(seed: u64, stream: u64) -> Self {
        Self(Self::mix(seed ^ stream.wrapping_mul(0x9e3779b97f4a7c15)))
    }

    pub fn choose(&mut self, alternatives: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        (Self::mix(self.0) % alternatives as u64) as usize
    }

    fn mix(mut value: u64) -> u64 {
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub alternatives: usize,
    pub selected: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Partition { socket: u64, blocked: bool },
    Reset { socket: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Connect(Address),
    Listen(Address, u16),
    Accept(u64),
    Recv(u64),
    Send(u64),
}
#[derive(Clone, Copy)]
enum Waiter {
    Network(Operation),
    Handshake(u64),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub value: u64,
    /// NetError declaration ordinal, zero for success.
    pub error: u8,
    pub code: u32,
    pub address: Address,
    /// Received plaintext or the selected ALPN protocol bytes.
    pub bytes: Vec<u8>,
    pub peer: [u8; 32],
}
impl Reply {
    fn error(error: u8) -> Self {
        Self {
            error,
            ..Self::default()
        }
    }
    fn handles() -> Self {
        Self {
            error: 8,
            code: 24,
            ..Self::default()
        }
    }
    fn value(value: u64) -> Self {
        Self {
            value,
            ..Self::default()
        }
    }
}
#[derive(Default)]
struct Direction {
    accepted: VecDeque<u8>,
    transit: VecDeque<u8>,
    readable: VecDeque<u8>,
    partitioned: bool,
    fin: bool,
}
impl Direction {
    fn is_drained(&self) -> bool {
        self.accepted.is_empty() && self.transit.is_empty() && self.readable.is_empty()
    }
}
struct Connection {
    directions: [Direction; 2],
    reset: bool,
    endpoints: [u64; 2],
    script: Option<Box<ScriptedPeer>>,
}
enum Record {
    Listener {
        address: Address,
        backlog: usize,
        pending: VecDeque<u64>,
    },
    Endpoint {
        connection: u64,
        side: usize,
        owners: u8,
        peer: Address,
    },
}
struct State {
    config: Config,
    now: u64,
    next: u64,
    port: u16,
    random: [SplitMix64; Choice::COUNT],
    decisions: BTreeMap<Choice, Vec<Decision>>,
    choice_indices: [usize; Choice::COUNT],
    liveness: bool,
    records: BTreeMap<u64, Record>,
    connections: BTreeMap<u64, Connection>,
    waiters: BTreeMap<u64, Waiter>,
    scripts: BTreeMap<Address, VecDeque<TlsScript>>,
    faults: Vec<(u64, Fault)>,
    delivery: Option<u64>,
}
/// Clones designate the same run's network and monotonic clock. Callers never
/// retain a lock while running a task or invoking a readiness callback.
#[derive(Clone)]
pub struct Model(Arc<Mutex<State>>);
impl Default for Model {
    fn default() -> Self {
        Self::new(Config::default())
    }
}
impl Model {
    pub fn new(config: Config) -> Self {
        assert!(config.queue_bytes > 0 && config.max_prefix > 0 && config.max_handles > 0);
        let random = std::array::from_fn(|kind| SplitMix64::new(config.seed, kind as u64));
        Self(Arc::new(Mutex::new(State {
            config,
            now: 0,
            next: 1,
            port: 49152,
            random,
            decisions: BTreeMap::new(),
            choice_indices: [0; Choice::COUNT],
            liveness: false,
            records: BTreeMap::new(),
            connections: BTreeMap::new(),
            waiters: BTreeMap::new(),
            scripts: BTreeMap::new(),
            faults: vec![],
            delivery: None,
        })))
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn owns(key: u64) -> bool {
        key >> 34 == 0
    }
    pub fn now(&self) -> u64 {
        self.state().now
    }
    pub fn clock_read(&self) -> u64 {
        let mut s = self.state();
        s.now = s.now.saturating_add(1);
        s.now
    }
    pub fn advance_to(&self, now: u64) {
        let mut s = self.state();
        s.now = s.now.max(now);
    }
    /// Harness choice in the given stream. Fault alternatives put the no-fault
    /// branch at zero, which repair selects without drawing or recording a choice.
    pub fn choose(&self, kind: Choice, alternatives: usize) -> usize {
        self.state().choose(kind, alternatives)
    }
    pub fn decisions(&self) -> BTreeMap<Choice, Vec<Decision>> {
        self.state().decisions.clone()
    }
    pub fn schedule(&self, at: u64, fault: Fault) {
        let mut s = self.state();
        if s.liveness {
            return;
        }
        let pos = s.faults.partition_point(|(time, _)| *time <= at);
        s.faults.insert(pos, (at, fault));
    }
    pub fn fault(&self, fault: Fault) {
        let mut s = self.state();
        if !s.liveness {
            s.fault(fault);
        }
    }
    /// Enters liveness mode permanently. Clears pending faults, heals partitions,
    /// and lets ordinary delivery drain the queues. Resets remain irreversible. Owners must still finish or cancel their operations.
    pub fn repair(&self) {
        let mut s = self.state();
        s.liveness = true;
        s.faults.clear();
        for c in s.connections.values_mut() {
            for d in &mut c.directions {
                d.partitioned = false;
            }
        }
        s.enable_delivery();
    }
    /// Queue a peer for the next connection to this address. Each script
    /// belongs to one connection and is released with its last socket owner.
    pub fn script_tls_peer(&self, address: Address, script: TlsScript) {
        self.state()
            .scripts
            .entry(address)
            .or_default()
            .push_back(script);
    }

    pub fn connected_peer(&self, key: u64) -> Option<Address> {
        match self.state().records.get(&key)? {
            Record::Endpoint {
                side: 0,
                owners: 1,
                peer,
                ..
            } => Some(*peer),
            _ => None,
        }
    }

    pub fn split(&self, key: u64) -> (u64, u64) {
        let mut s = self.state();
        let Record::Endpoint { owners, .. } = s.records.get_mut(&(key & !3)).unwrap() else {
            unreachable!()
        };
        *owners = 6;
        (key | 1, key | 2)
    }
    pub fn release(&self, key: u64) {
        self.state().release(key);
    }
    pub(crate) fn release_all(&self, key: u64) {
        let mut s = self.state();
        let owners = match &s.records[&key] {
            Record::Endpoint { owners, .. } => *owners,
            Record::Listener { .. } => 1,
        };
        for direction in 0..3 {
            if owners & (1 << direction) != 0 {
                s.release(key | direction);
            }
        }
    }
    pub fn live_handles(&self) -> usize {
        self.state().records.len()
    }
    pub fn contains(&self, key: u64) -> bool {
        self.state().records.contains_key(&(key & !3))
    }
    pub fn live_tls_sessions(&self) -> usize {
        self.state()
            .connections
            .values()
            .filter(|c| c.script.is_some())
            .count()
    }
    pub fn pending(&self) -> usize {
        self.state().waiters.len()
    }
    pub fn queued_bytes(&self, socket: u64) -> [usize; 3] {
        let s = self.state();
        let (id, side) = s.endpoint(socket);
        let d = &s.connections[&id].directions[side];
        [d.accepted.len(), d.transit.len(), d.readable.len()]
    }
    pub fn attempt(&self, op: Operation, input: &[u8], spare: usize, token: u64) -> Option<Reply> {
        self.attempt_parts(op, [input, &[]], spare, token)
    }

    pub fn attempt_parts(
        &self,
        op: Operation,
        input: [&[u8]; 2],
        spare: usize,
        token: u64,
    ) -> Option<Reply> {
        self.attempt_with(Waiter::Network(op), token, |s| s.attempt(op, input, spare))
    }

    pub fn attempt_secret(
        &self,
        key: u64,
        input: [&[u8]; 3],
        committed: &mut usize,
        token: u64,
    ) -> Option<Reply> {
        let op = Operation::Send(key);
        self.attempt_with(Waiter::Network(op), token, |state| {
            let mut skip = *committed;
            for part in input {
                if skip >= part.len() {
                    skip -= part.len();
                    continue;
                }
                let remaining = &part[skip..];
                let reply = state.attempt(op, [remaining, &[]], 0)?;
                if reply.error != 0 {
                    return Some(reply);
                }
                *committed += reply.value as usize;
                if reply.value < remaining.len() as u64 {
                    return None;
                }
                skip = 0;
            }
            state.attempt(op, [&[], &[]], 0)
        })
    }

    pub fn attempt_tls(&self, key: u64, offered: &[impl AsRef<[u8]>], token: u64) -> Option<Reply> {
        self.attempt_with(Waiter::Handshake(key), token, |s| s.handshake(key, offered))
    }

    fn attempt_with(
        &self,
        waiter: Waiter,
        token: u64,
        attempt: impl FnOnce(&mut State) -> Option<Reply>,
    ) -> Option<Reply> {
        let mut s = self.state();
        s.waiters.remove(&token);
        s.events();
        let reply = attempt(&mut s);
        if reply.is_none() {
            s.waiters.insert(token, waiter);
        }
        reply
    }

    pub fn abort(&self, token: u64, cancelled: bool) -> Reply {
        self.state().waiters.remove(&token);
        Reply::error(if cancelled { 7 } else { 6 })
    }
    /// Poll due events; while idle jump to the first delivery, fault or deadline.
    /// Ready tokens are in operation-token order. They are removed before return,
    /// so callbacks may re-arm without duplicating a wake from this poll.
    pub fn poll(&self, deadline: Option<u64>, idle: bool) -> Vec<u64> {
        let mut s = self.state();
        s.events();
        if idle && !s.waiters.values().any(|&waiter| s.waiter_ready(waiter)) {
            let handshake = s
                .waiters
                .values()
                .filter_map(|op| match *op {
                    Waiter::Handshake(key) => s.connections[&key].script.as_ref()?.ready_at(),
                    _ => None,
                })
                .min();
            let next = [
                s.delivery,
                s.faults.first().map(|(at, _)| *at),
                deadline,
                handshake,
            ]
            .into_iter()
            .flatten()
            .min();
            if let Some(next) = next {
                s.now = s.now.max(next);
                s.events();
            }
        }
        let ready: Vec<_> = s
            .waiters
            .iter()
            .filter_map(|(&token, &waiter)| s.waiter_ready(waiter).then_some(token))
            .collect();
        for token in &ready {
            s.waiters.remove(token);
        }
        ready
    }
}
impl State {
    fn choose(&mut self, kind: Choice, alternatives: usize) -> usize {
        assert!(alternatives > 0);
        if kind == Choice::Fault && self.liveness {
            return 0;
        }
        let index = &mut self.choice_indices[kind as usize];
        let selected = self
            .config
            .prefix
            .get(&kind)
            .and_then(|prefix| prefix.get(*index))
            .copied()
            .unwrap_or_else(|| self.random[kind as usize].choose(alternatives));
        assert!(
            selected < alternatives,
            "choice prefix outside this branch's bounds"
        );
        *index = index.checked_add(1).expect("model choice index exhausted");
        if self.config.record_decisions {
            self.decisions.entry(kind).or_default().push(Decision {
                alternatives,
                selected,
            });
        }
        selected
    }
    fn key(&mut self) -> u64 {
        let next = self.next;
        self.next += 1;
        next << 2
    }
    fn endpoint(&self, key: u64) -> (u64, usize) {
        match self.records[&(key & !3)] {
            Record::Endpoint {
                connection, side, ..
            } => (connection, side),
            _ => unreachable!(),
        }
    }
    fn matching_listener(&self, a: Address) -> Option<u64> {
        self.records.iter().find_map(|(&key, r)| match r {
            Record::Listener { address, .. }
                if address.family == a.family
                    && address.port == a.port
                    && ((*address == a) || (address.hi == 0 && address.lo == 0)) =>
            {
                Some(key)
            }
            _ => None,
        })
    }
    fn ephemeral(&mut self, mut a: Address) -> Option<Address> {
        for _ in 49152..=u16::MAX {
            a.port = self.port;
            self.port = if self.port == u16::MAX {
                49152
            } else {
                self.port + 1
            };
            if self.matching_listener(a).is_none() {
                return Some(a);
            }
        }
        None
    }
    fn attempt(&mut self, op: Operation, input: [&[u8]; 2], spare: usize) -> Option<Reply> {
        match op {
            Operation::Listen(mut address, backlog) => {
                if address.port == 0 {
                    let Some(a) = self.ephemeral(address) else {
                        return Some(Reply::error(4));
                    };
                    address = a;
                }
                let overlaps = self.records.values().any(|r| matches!(r, Record::Listener { address: a, .. }
                    if a.family == address.family && a.port == address.port && (*a == address || (a.hi == 0 && a.lo == 0) || (address.hi == 0 && address.lo == 0))));
                if overlaps {
                    return Some(Reply::error(4));
                }
                if self.records.len() >= self.config.max_handles || self.next > LAST_ID {
                    return Some(Reply::handles());
                }
                let key = self.key();
                self.records.insert(
                    key,
                    Record::Listener {
                        address,
                        backlog: usize::from(backlog).max(1),
                        pending: VecDeque::new(),
                    },
                );
                Some(Reply {
                    address,
                    ..Reply::value(key)
                })
            }
            Operation::Connect(address) => {
                let scripted = self.scripts.contains_key(&address);
                let listener = self.matching_listener(address);
                if !scripted && listener.is_none() {
                    return Some(Reply::error(2));
                }
                if !self.ready(op) {
                    return None;
                }
                let handles = if scripted { 1 } else { 2 };
                if self.records.len().saturating_add(handles) > self.config.max_handles
                    || self.next + handles as u64 - 1 > LAST_ID
                {
                    return Some(Reply::handles());
                }
                if scripted {
                    let scripts = self.scripts.get_mut(&address).unwrap();
                    let script = scripts.pop_front().unwrap();
                    if scripts.is_empty() {
                        self.scripts.remove(&address);
                    }
                    let client = self.key();
                    self.connections.insert(
                        client,
                        Connection {
                            directions: Default::default(),
                            reset: false,
                            endpoints: [client, 0],
                            script: Some(Box::new(ScriptedPeer::new(script))),
                        },
                    );
                    self.records.insert(
                        client,
                        Record::Endpoint {
                            connection: client,
                            side: 0,
                            owners: 1,
                            peer: address,
                        },
                    );
                    return Some(Reply::value(client));
                }
                let listener = listener.unwrap();
                let mut peer = address;
                if peer.family == 0 {
                    peer.hi = 0;
                    peer.lo = 0x7f000001;
                } else {
                    peer.hi = 0;
                    peer.lo = 1;
                }
                let Some(peer) = self.ephemeral(peer) else {
                    return Some(Reply::handles());
                };
                let client = self.key();
                let server = self.key();
                self.connections.insert(
                    client,
                    Connection {
                        directions: Default::default(),
                        reset: false,
                        endpoints: [client, server],
                        script: None,
                    },
                );
                self.records.insert(
                    client,
                    Record::Endpoint {
                        connection: client,
                        side: 0,
                        owners: 1,
                        peer: address,
                    },
                );
                self.records.insert(
                    server,
                    Record::Endpoint {
                        connection: client,
                        side: 1,
                        owners: 1,
                        peer,
                    },
                );
                let Record::Listener { pending, .. } = self.records.get_mut(&listener).unwrap()
                else {
                    unreachable!()
                };
                pending.push_back(server);
                Some(Reply::value(client))
            }
            Operation::Accept(key) => {
                let Record::Listener { pending, .. } = self.records.get_mut(&key).unwrap() else {
                    unreachable!()
                };
                let key = pending.pop_front()?;
                let Record::Endpoint { peer, .. } = self.records[&key] else {
                    unreachable!()
                };
                Some(Reply {
                    address: peer,
                    ..Reply::value(key)
                })
            }
            Operation::Send(key) => {
                let [prefix, tail] = input;
                let len = prefix.len() + tail.len();
                let (id, side) = self.endpoint(key);
                let c = &self.connections[&id];
                let tls = c.script.as_ref().is_some_and(|p| p.established());
                if len == 0 && !tls {
                    return Some(Reply::value(0));
                }
                if c.reset {
                    return Some(Reply::error(5));
                }
                let queued = c.directions[side].accepted.len();
                let available = if tls {
                    if queued != 0 {
                        return None;
                    }
                    if len == 0 {
                        return Some(Reply::value(0));
                    }
                    self.config.queue_bytes.min(RECORD_BYTES)
                } else {
                    self.config.queue_bytes - queued
                };
                if available == 0 {
                    return None;
                }
                let bound = available.min(len).min(self.config.max_prefix);
                let n = self.choose(Choice::Send, bound) + 1;
                let first = n.min(prefix.len());
                let accepted =
                    &mut self.connections.get_mut(&id).unwrap().directions[side].accepted;
                accepted.extend(&prefix[..first]);
                accepted.extend(&tail[..n - first]);
                self.enable_delivery();
                Some(Reply::value(n as u64))
            }
            Operation::Recv(key) => {
                let (id, side) = self.endpoint(key);
                let c = self.connections.get_mut(&id).unwrap();
                let d = &c.directions[1 - side];
                let drained = d.is_drained();
                if let Some(error) = c
                    .script
                    .as_mut()
                    .and_then(|p| p.receive_error(drained, spare))
                {
                    return Some(error);
                }
                if spare == 0 {
                    return Some(Reply::value(0));
                }
                if !d.readable.is_empty() {
                    let bound = spare.min(d.readable.len()).min(self.config.max_prefix);
                    let n = self.choose(Choice::Receive, bound) + 1;
                    let bytes = self.connections.get_mut(&id).unwrap().directions[1 - side]
                        .readable
                        .drain(..n)
                        .collect();
                    self.enable_delivery();
                    return Some(Reply {
                        bytes,
                        ..Reply::value(n as u64)
                    });
                }
                if c.reset {
                    return Some(Reply::error(5));
                }
                if d.fin && drained {
                    return Some(Reply::value(0));
                }
                None
            }
        }
    }
    fn handshake(&mut self, key: u64, offered: &[impl AsRef<[u8]>]) -> Option<Reply> {
        let c = self.connections.get_mut(&key).unwrap();
        if c.reset {
            return Some(Reply::error(5));
        }
        let Some(peer) = &mut c.script else {
            return Some(tls::unsupported());
        };
        let reply = peer.establish(self.now, key, offered)?;
        if reply.error == 0 {
            peer.drive(&mut c.directions, self.config.queue_bytes);
            self.enable_delivery();
        }
        Some(reply)
    }

    fn waiter_ready(&self, waiter: Waiter) -> bool {
        match waiter {
            Waiter::Network(op) => self.ready(op),
            Waiter::Handshake(key) => {
                let c = &self.connections[&key];
                c.reset
                    || c.script
                        .as_ref()
                        .and_then(|p| p.ready_at())
                        .is_some_and(|at| at <= self.now)
            }
        }
    }
    fn ready(&self, op: Operation) -> bool {
        match op {
            Operation::Listen(..) => true,
            Operation::Connect(address) => {
                self.scripts.contains_key(&address)
                    || self.matching_listener(address).is_none_or(|key| {
                        matches!(&self.records[&key], Record::Listener { pending, backlog, .. } if pending.len() < *backlog)
                    })
            }
            Operation::Accept(key) => matches!(&self.records[&key], Record::Listener { pending, .. } if !pending.is_empty()),
            Operation::Send(key) => {
                let (id, side) = self.endpoint(key);
                let c = &self.connections[&id];
                let queued = c.directions[side].accepted.len();
                c.reset || if c.script.as_ref().is_some_and(|p| p.established()) {
                    queued == 0
                } else {
                    queued < self.config.queue_bytes
                }
            }
            Operation::Recv(key) => {
                let (id, side) = self.endpoint(key);
                let c = &self.connections[&id];
                let d = &c.directions[1 - side];
                let drained = d.is_drained();
                c.reset || !d.readable.is_empty() || (d.fin && drained)
                    || c.script.as_ref().is_some_and(|p| p.read_terminal(drained).is_some())
            }
        }
    }
    fn can_deliver(&self) -> bool {
        self.connections.values().any(|c| {
            c.directions.iter().any(|d| {
                !d.partitioned
                    && ((!d.accepted.is_empty() && d.transit.len() < self.config.queue_bytes)
                        || (!d.transit.is_empty() && d.readable.len() < self.config.queue_bytes))
            })
        })
    }
    fn enable_delivery(&mut self) {
        if self.delivery.is_none() && self.can_deliver() {
            self.delivery = Some(self.now.saturating_add(self.config.latency_ns));
        }
    }
    fn events(&mut self) {
        while self.faults.first().is_some_and(|(at, _)| *at <= self.now) {
            let (_, fault) = self.faults.remove(0);
            self.fault(fault);
        }
        while self.delivery.is_some_and(|at| at <= self.now) {
            self.delivery = None;
            let keys: Vec<_> = self.connections.keys().copied().collect();
            for key in keys {
                for side in 0..2 {
                    if self.connections[&key].directions[side].partitioned {
                        continue;
                    }
                    for transit in [true, false] {
                        let d = &self.connections[&key].directions[side];
                        let bound = if transit {
                            d.transit
                                .len()
                                .min(self.config.queue_bytes - d.readable.len())
                        } else {
                            d.accepted
                                .len()
                                .min(self.config.queue_bytes - d.transit.len())
                        };
                        if bound == 0 {
                            continue;
                        }
                        let n = self.choose(Choice::Delivery, bound) + 1;
                        let d = &mut self.connections.get_mut(&key).unwrap().directions[side];
                        if transit {
                            d.readable.extend(d.transit.drain(..n));
                        } else {
                            d.transit.extend(d.accepted.drain(..n));
                        }
                    }
                }
            }
            for c in self.connections.values_mut() {
                if !c.reset
                    && let Some(peer) = &mut c.script
                {
                    peer.drive(&mut c.directions, self.config.queue_bytes);
                }
            }
            self.enable_delivery();
        }
    }
    fn fault(&mut self, fault: Fault) {
        let key = match fault {
            Fault::Partition { socket, .. } | Fault::Reset { socket } => socket,
        };
        let (id, side) = self.endpoint(key);
        let c = self.connections.get_mut(&id).unwrap();
        match fault {
            Fault::Partition { blocked, .. } => c.directions[side].partitioned = blocked,
            Fault::Reset { .. } => {
                c.reset = true;
                for d in &mut c.directions {
                    d.accepted.clear();
                    d.transit.clear();
                }
            }
        }
        self.enable_delivery();
    }
    fn release(&mut self, key: u64) {
        let base = key & !3;
        let record = self.records.get_mut(&base).unwrap();
        if let Record::Endpoint {
            connection,
            side,
            owners,
            ..
        } = record
        {
            let (connection, side) = (*connection, *side);
            if key & 3 == 0 || key & 3 == 2 {
                let c = self.connections.get_mut(&connection).unwrap();
                let direction = &mut c.directions[side];
                direction.fin = true;
                if let Some(peer) = &mut c.script
                    && peer.established()
                {
                    // One nonblocking flush before shutdown. Only bytes
                    // accepted by the transport survive a blocked drop.
                    if !direction.partitioned && !c.reset {
                        let n = direction
                            .accepted
                            .len()
                            .min(self.config.queue_bytes - direction.transit.len());
                        direction.transit.extend(direction.accepted.drain(..n));
                    }
                    direction.accepted.clear();
                    if !c.reset {
                        peer.drive(&mut c.directions, self.config.queue_bytes);
                    }
                }
            }
            *owners &= !(1 << (key & 3));
            if *owners != 0 {
                return;
            }
        }
        match self.records.remove(&base).unwrap() {
            Record::Listener { pending, .. } => {
                for key in pending {
                    self.fault(Fault::Reset { socket: key });
                    self.release(key);
                }
            }
            Record::Endpoint {
                connection, side, ..
            } => {
                let c = self.connections.get_mut(&connection).unwrap();
                c.endpoints[side] = 0;
                if c.endpoints == [0, 0] {
                    self.connections.remove(&connection);
                }
            }
        }
        self.enable_delivery();
    }
}

#[cfg(test)]
mod tests;
