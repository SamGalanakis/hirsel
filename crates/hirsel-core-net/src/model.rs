//! Deterministic network and clock, shared by evaluator and native providers.
//! Adapted from FoundationDB sim2.cpp's scheduled delivery and simulated time.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

// Host registry generations start at one. Model identities use generation zero.
const LAST_ID: u64 = u32::MAX as u64;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    pub prefix: Vec<usize>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            queue_bytes: 8,
            max_prefix: 8,
            max_handles: 256,
            latency_ns: 0,
            seed: 1,
            prefix: vec![],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Send,
    Receive,
    Delivery,
    Fault,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub kind: Choice,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub value: u64,
    /// NetError declaration ordinal, zero for success.
    pub error: u8,
    pub code: u32,
    pub address: Address,
    pub bytes: Vec<u8>,
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
struct Connection {
    directions: [Direction; 2],
    reset: bool,
    endpoints: [u64; 2],
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
    random: u64,
    decisions: Vec<Decision>,
    records: BTreeMap<u64, Record>,
    connections: BTreeMap<u64, Connection>,
    waiters: BTreeMap<u64, Operation>,
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
        let random = config.seed;
        Self(Arc::new(Mutex::new(State {
            config,
            now: 0,
            next: 1,
            port: 49152,
            random,
            decisions: vec![],
            records: BTreeMap::new(),
            connections: BTreeMap::new(),
            waiters: BTreeMap::new(),
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
    /// Harness choice, recorded alongside send/receive/delivery choices. Fault
    /// exploration uses it at each reached branch, including branches in retries.
    pub fn choose(&self, kind: Choice, alternatives: usize) -> usize {
        self.state().choose(kind, alternatives)
    }
    pub fn decisions(&self) -> Vec<Decision> {
        self.state().decisions.clone()
    }
    pub fn schedule(&self, at: u64, fault: Fault) {
        let mut s = self.state();
        let pos = s.faults.partition_point(|(time, _)| *time <= at);
        s.faults.insert(pos, (at, fault));
    }
    pub fn fault(&self, fault: Fault) {
        self.state().fault(fault);
    }
    /// Stops future faults, repairs every direction, and lets normal delivery
    /// drain the queues. Owners must still finish or cancel their operations.
    pub fn repair(&self) {
        let mut s = self.state();
        s.faults.clear();
        for c in s.connections.values_mut() {
            for d in &mut c.directions {
                d.partitioned = false;
            }
        }
        s.enable_delivery();
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
        let mut s = self.state();
        s.waiters.remove(&token);
        s.events();
        let reply = s.attempt(op, input, spare);
        if reply.is_none() {
            s.waiters.insert(token, op);
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
        if idle && !s.waiters.values().any(|&op| s.ready(op)) {
            let next = [s.delivery, s.faults.first().map(|(at, _)| *at), deadline]
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
            .filter_map(|(&token, &op)| s.ready(op).then_some(token))
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
        // SplitMix64 is deterministic even for seed zero.
        self.random = self.random.wrapping_add(0x9e3779b97f4a7c15);
        let mut random = self.random;
        random = (random ^ (random >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        random = (random ^ (random >> 27)).wrapping_mul(0x94d049bb133111eb);
        random ^= random >> 31;
        let selected = self
            .config
            .prefix
            .get(self.decisions.len())
            .copied()
            .unwrap_or((random as usize) % alternatives);
        assert!(
            selected < alternatives,
            "choice prefix outside this branch's bounds"
        );
        self.decisions.push(Decision {
            kind,
            alternatives,
            selected,
        });
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
    fn attempt(&mut self, op: Operation, input: &[u8], spare: usize) -> Option<Reply> {
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
                let Some(listener) = self.matching_listener(address) else {
                    return Some(Reply::error(2));
                };
                if !self.ready(op) {
                    return None;
                }
                if self.records.len().saturating_add(2) > self.config.max_handles
                    || self.next >= LAST_ID
                {
                    return Some(Reply::handles());
                }
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
                if input.is_empty() {
                    return Some(Reply::value(0));
                }
                let (id, side) = self.endpoint(key);
                let c = &self.connections[&id];
                if c.reset {
                    return Some(Reply::error(5));
                }
                let available = self.config.queue_bytes - c.directions[side].accepted.len();
                if available == 0 {
                    return None;
                }
                let bound = available.min(input.len()).min(self.config.max_prefix);
                let n = self.choose(Choice::Send, bound) + 1;
                self.connections.get_mut(&id).unwrap().directions[side]
                    .accepted
                    .extend(&input[..n]);
                self.enable_delivery();
                Some(Reply::value(n as u64))
            }
            Operation::Recv(key) => {
                if spare == 0 {
                    return Some(Reply::value(0));
                }
                let (id, side) = self.endpoint(key);
                let c = &self.connections[&id];
                let d = &c.directions[1 - side];
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
                if d.fin && d.accepted.is_empty() && d.transit.is_empty() {
                    return Some(Reply::value(0));
                }
                None
            }
        }
    }
    fn ready(&self, op: Operation) -> bool {
        match op {
            Operation::Listen(..) => true,
            Operation::Connect(address) => self.matching_listener(address).is_none_or(|key| {
                matches!(&self.records[&key], Record::Listener { pending, backlog, .. } if pending.len() < *backlog)
            }),
            Operation::Accept(key) => matches!(&self.records[&key], Record::Listener { pending, .. } if !pending.is_empty()),
            Operation::Send(key) => { let (id, side) = self.endpoint(key); let c = &self.connections[&id];
                c.reset || c.directions[side].accepted.len() < self.config.queue_bytes },
            Operation::Recv(key) => { let (id, side) = self.endpoint(key); let c = &self.connections[&id]; let d = &c.directions[1-side];
                c.reset || !d.readable.is_empty() || (d.fin && d.accepted.is_empty() && d.transit.is_empty()) },
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
                self.connections.get_mut(&connection).unwrap().directions[side].fin = true;
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
