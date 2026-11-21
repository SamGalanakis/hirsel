//! Units and connections on the evaluator (CONC-15, CONC-17).
//!
//! The evaluator uses one scheduler for every unit. It keeps each unit's
//! connection arrivals in order and applies them at that unit's scheduler
//! turns and at the waits and empty receives that take arrivals natively.
use super::*;
use hirsel_core_layout::cross::{self, CLIENT, SEQ, SOCKET};

/// Client of connection `i` is `CONN_BASE + 2i`, its inbox `CONN_BASE + 2i + 1`.
pub(super) const CONN_BASE: u64 = 0x80_0000;

pub(super) struct Conn {
    item: TypeRef,
    owner: usize,
    cap: u64,
    room: u64,
    used: u64,
    queue: VecDeque<Item>,
    taken: usize,
    closed: bool,
    /// Tickets of waiting sends, oldest first.
    waiting: VecDeque<u64>,
    tickets: u64,
    clients: u64,
    inbox: bool,
    charge: u64,
    limit: Option<Limit>,
}

pub(super) enum UnitNotice {
    Item(usize),
    Close(usize),
    Refund(Limit, u64),
    HelperEnd(usize),
}

pub(super) struct Item {
    bytes: Vec<u8>,
    seqs: Vec<Vec<u8>>,
    pub(super) charge: u64,
}

fn conn(handle: u64) -> usize {
    ((handle - CONN_BASE) / 2) as usize
}

impl Machine<'_> {
    pub(super) fn take_unit_notices(&mut self) {
        let unit = self.sched.tasks[self.sched.current].unit;
        while let Some(notice) = self.sched.tasks[unit].notices.pop_front() {
            match notice {
                UnitNotice::Item(i) => self.conns[i].taken += 1,
                UnitNotice::Close(i) => self.conns[i].closed = true,
                UnitNotice::Refund(limit, amount) => limit.find().subtract(amount),
                UnitNotice::HelperEnd(i) => self.end_take(unit, i),
            }
        }
    }

    /// The unit a notice for `unit` reaches: an ended helper's notices go to
    /// its owner once the owner has taken its end (native `Port::fwd`).
    pub(super) fn unit_dest(&self, mut unit: usize) -> usize {
        while let Some(c) = unit.checked_sub(1).and_then(|i| self.children.get(i))
            && c.end_taken
            && let Some(owner) = c.unit_owner
        {
            unit = owner;
        }
        unit
    }

    pub(super) fn notify(&mut self, unit: usize, notice: UnitNotice) {
        let to = self.unit_dest(unit);
        self.sched.tasks[to].notices.push_back(notice);
    }

    /// A helper's end take: the owner gets back the task record, the unused
    /// part of `n` and the unused primary cell, and keeps `size(R)`, the
    /// result's content and the connections the helper left behind. Those
    /// are charged to the helper's limit, which now forwards to the owner's,
    /// so each refunds there at its own retirement. The notices still queued
    /// at the helper follow, in order (native `end_take`).
    fn end_take(&mut self, unit: usize, i: usize) {
        let c = &mut self.children[i];
        c.end_taken = true;
        let lim = c.unit_limit.take().expect("a helper's limit");
        let survivors = lim.used();
        let result = c.crossed_result.as_ref().map_or(0, |item| item.charge);
        let r_size = c.charge - c.n - charge::TASK_RECORD - charge::CAUSE_CELL;
        let back = c.charge - r_size - charge::CAUSE_CELL - result - survivors;
        c.end_kept = r_size + result;
        let payer = c.limit.clone().unwrap();
        let primary = c.primary.take();
        {
            let mut state = lim.0.state.lock().unwrap();
            state.parent = Some(payer.clone());
            state.ended = true;
        }
        self.refund(payer, back);
        if let Some(p) = primary {
            self.refund(p.limit, p.amount);
        }
        let rest = std::mem::take(&mut self.sched.tasks[i + 1].notices);
        self.sched.tasks[unit].notices.extend(rest);
    }

    pub(super) fn cross_owners(&self, ty: TypeRef) -> Vec<[u64; 4]> {
        cross::owners(&self.m.types, &self.recipes, ty)
    }

    /// What the value at `at` carries across: its buffers at their length.
    pub(super) fn cross_charge(&self, at: u64, owners: &[[u64; 4]]) -> u64 {
        owners
            .iter()
            .filter(|o| o[1] == SEQ && o[2] != 0)
            .map(|o| charge::seq(self.load(at + o[0] + 8, 8), o[3]).unwrap_or(u64::MAX))
            .fold(0u64, u64::saturating_add)
    }

    /// Copy the value at `at` out and free its buffers to their records; its
    /// clients move with the bytes.
    pub(super) fn cross_take(&mut self, at: u64, ty: TypeRef, owners: &[[u64; 4]]) -> Item {
        let charge = self.cross_charge(at, owners);
        let bytes = self.get(at, self.held(ty) as usize).to_vec();
        let mut seqs = Vec::new();
        for o in owners.iter().filter(|o| o[1] == SEQ && o[2] != 0) {
            let carrier = at + o[0];
            let (p, len, cap) = (
                self.load(carrier, 8),
                self.load(carrier + 8, 8),
                self.load(carrier + 16, 8),
            );
            let n = (len * o[2]) as usize;
            seqs.push(if n == 0 {
                Vec::new()
            } else {
                self.get(p, n).to_vec()
            });
            // Capacity 0 is empty or static data the carrier does not own.
            if cap != 0 {
                self.free_as(p, charge::seq(cap, o[3]), None, "a crossing buffer");
            }
        }
        Item {
            bytes,
            seqs,
            charge,
        }
    }

    /// Write `item` at `dest`, each buffer fresh at its length and charged to
    /// `limit`, which has admitted `item.charge`.
    pub(super) fn cross_put(&mut self, dest: u64, item: &Item, owners: &[[u64; 4]], limit: &Limit) {
        self.put(dest, &item.bytes);
        let seqs = owners.iter().filter(|o| o[1] == SEQ && o[2] != 0);
        for (o, content) in seqs.zip(&item.seqs) {
            let carrier = dest + o[0];
            let len = self.load(carrier + 8, 8);
            if len == 0 {
                self.store(carrier, 8, 0);
                self.store(carrier + 16, 8, 0);
                continue;
            }
            let c = charge::seq(len, o[3]).expect("an admitted buffer");
            let b = self.block(content.len() as u64, Some(limit.clone()), c, None);
            self.put(b, content);
            self.store(carrier, 8, b);
            self.store(carrier + 16, 8, len);
        }
    }

    /// Drop the handles owned by a crossing value that nobody took.
    pub(super) fn cross_discard(&mut self, item: &Item, owners: &[[u64; 4]]) {
        for o in owners.iter().filter(|o| matches!(o[1], CLIENT | SOCKET)) {
            let at = o[0] as usize;
            let key = u64::from_le_bytes(item.bytes[at..at + 8].try_into().unwrap());
            match o[1] {
                CLIENT => self.client_drop(key),
                SOCKET => self.socket_drop(key),
                _ => unreachable!(),
            }
        }
    }

    pub(super) fn conn_new(
        &mut self,
        cur: &Limit,
        item: TypeId,
        cap: u64,
        room: u64,
    ) -> X<Vec<u64>> {
        let item = TypeRef(item.0);
        if cap == 0 {
            return Err(Stop::Cause("Bounds"));
        }
        let charge = cap
            .checked_mul(self.logical(item))
            .and_then(|n| n.checked_add(charge::CONNECTION_BASE))
            .and_then(|n| n.checked_add(room))
            .unwrap_or(u64::MAX);
        if let Err(available) = self.admit(cur, charge) {
            return Ok(vec![0, 0, available]);
        }
        self.conns.push(Conn {
            item,
            owner: self.sched.tasks[self.sched.current].unit,
            cap,
            room,
            used: 0,
            queue: VecDeque::new(),
            taken: 0,
            closed: false,
            waiting: VecDeque::new(),
            tickets: 0,
            clients: 1,
            inbox: true,
            charge,
            limit: Some(cur.clone()),
        });
        let c = CONN_BASE + 2 * (self.conns.len() as u64 - 1);
        Ok(vec![c, c + 1, 0])
    }

    pub(super) fn conn_client(&mut self, client: u64) -> Vec<u64> {
        self.conns[conn(client)].clients += 1;
        vec![client]
    }

    /// Whether connection `i` takes an item of `charge` now; a waiting send
    /// passes only from the front of the waiting sends.
    fn conn_fits(&self, i: usize, charge: u64, ticket: Option<u64>) -> bool {
        let c = &self.conns[i];
        let first = match ticket {
            Some(t) => c.waiting.front() == Some(&t),
            None => c.waiting.is_empty(),
        };
        first && (c.queue.len() as u64) < c.cap && charge <= c.room - c.used
    }

    fn conn_accept(&mut self, i: usize, at: u64, owners: &[[u64; 4]]) {
        let item = self.cross_take(at, self.conns[i].item, owners);
        let c = &mut self.conns[i];
        c.used += item.charge;
        c.queue.push_back(item);
        let owner = c.owner;
        self.notify(owner, UnitNotice::Item(i));
        self.wake(Condition::ConnReceive(i));
    }

    /// `conn.send` (`wait`) and `conn.try_send`: 0 accepted, 1 closed, 2 full.
    pub(super) fn conn_send(&mut self, t: &Task, wait: bool, client: u64, at: u64) -> X<Vec<u64>> {
        let i = conn(client);
        if wait {
            self.check_cancel(t)?;
        }
        let owners = self.cross_owners(self.conns[i].item);
        let charge = self.cross_charge(at, &owners);
        if charge > self.conns[i].room {
            return Err(Stop::Cause("Bounds"));
        }
        if !self.conns[i].inbox {
            return Ok(vec![1]);
        }
        if self.conn_fits(i, charge, None) {
            self.conn_accept(i, at, &owners);
            return Ok(vec![0]);
        }
        if !wait {
            self.event(TRY_SEND, client, 1);
            return Ok(vec![2]);
        }
        let ticket = self.conns[i].tickets;
        self.conns[i].tickets += 1;
        self.conns[i].waiting.push_back(ticket);
        loop {
            self.take_unit_notices();
            // A withdrawal that wins fails the task with `Cancelled`.
            if let Err(stop) = self.check_cancel(t) {
                self.conns[i].waiting.retain(|&w| w != ticket);
                self.wake(Condition::ConnSend(i));
                return Err(stop);
            }
            if !self.conns[i].inbox {
                self.conns[i].waiting.retain(|&w| w != ticket);
                return Ok(vec![1]);
            }
            if self.conn_fits(i, charge, Some(ticket)) {
                self.conns[i].waiting.pop_front();
                self.conn_accept(i, at, &owners);
                // The next oldest may fit too.
                self.wake(Condition::ConnSend(i));
                return Ok(vec![0]);
            }
            self.park_task(t, Condition::ConnSend(i));
        }
    }

    /// `conn.receive` (`wait`) and `conn.try_receive`: `(outcome, requested,
    /// available)`, outcome 0 an item, 1 closed, 2 `Capacity`, 3 empty.
    pub(super) fn conn_receive(
        &mut self,
        t: &Task,
        cur: &Limit,
        wait: bool,
        inbox: u64,
        dest: u64,
    ) -> X<Vec<u64>> {
        let i = conn(inbox);
        loop {
            if wait {
                self.check_cancel(t)?;
            }
            if self.conns[i].taken == 0 {
                self.take_unit_notices();
            }
            if self.conns[i].taken != 0 {
                let charge = self.conns[i].queue.front().unwrap().charge;
                if let Err(available) = self.admit(cur, charge) {
                    return Ok(vec![2, charge, available]);
                }
                let item = self.conns[i].queue.pop_front().expect("the front item");
                let owners = self.cross_owners(self.conns[i].item);
                self.cross_put(dest, &item, &owners, cur);
                self.conns[i].used -= charge;
                self.conns[i].taken -= 1;
                self.wake(Condition::ConnSend(i));
                return Ok(vec![0, 0, 0]);
            }
            if self.conns[i].closed {
                return Ok(vec![1, 0, 0]);
            }
            if !wait {
                self.event(TRY_RECEIVE, inbox, 1);
                return Ok(vec![3, 0, 0]);
            }
            self.park_task(t, Condition::ConnReceive(i));
        }
    }

    pub(super) fn client_drop(&mut self, client: u64) {
        let i = conn(client);
        self.conns[i].clients -= 1;
        let c = &self.conns[i];
        if c.clients == 0 && c.inbox {
            let owner = c.owner;
            self.notify(owner, UnitNotice::Close(i));
            self.wake(Condition::ConnReceive(i));
        }
        self.conn_freed(i);
    }

    /// Cleaning an inbox seals its connection, settles its waiting sends
    /// `Closed`, and drops what was not received, last accepted first.
    pub(super) fn inbox_drop(&mut self, inbox: u64) {
        let i = conn(inbox);
        self.conns[i].inbox = false;
        self.take_unit_notices();
        self.wake(Condition::ConnSend(i));
        let owners = self.cross_owners(self.conns[i].item);
        while let Some(item) = self.conns[i].queue.pop_back() {
            self.conns[i].used -= item.charge;
            self.cross_discard(&item, &owners);
        }
        self.conns[i].taken = 0;
        self.conn_freed(i);
        self.take_unit_notices();
    }

    /// A retirement travels with its notice to the owner. A peer's release
    /// becomes spendable at the owner's next take; the owner's own release is
    /// a take for its own retirement, through its notice in order.
    fn conn_freed(&mut self, i: usize) {
        let c = &mut self.conns[i];
        if c.clients == 0
            && !c.inbox
            && let Some(limit) = c.limit.take()
        {
            let (owner, charge) = (c.owner, c.charge);
            self.notify(owner, UnitNotice::Refund(limit, charge));
            if self.unit_dest(owner) == self.sched.tasks[self.sched.current].unit {
                self.take_unit_notices();
            }
        }
    }
}
