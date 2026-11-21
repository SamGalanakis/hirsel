//! Units and connections on the evaluator (CONC-15, CONC-17).
//!
//! The evaluator runs every unit's tasks on its one scheduler, in its one event
//! order, so a helper unit is a task whose entry and result cross as the native
//! runtime's do: the owners `cross` allows (sequence buffers and clients) are
//! listed by `hirsel_core_layout::cross::owners`; a buffer's content is copied
//! into the receiving side at its length and the original freed, and a client
//! moves. A connection is a queue of `cap` slots and a room of `b` bytes; an item
//! is charged to the room from acceptance until a receive admits it into the
//! receiving limit. Refunds and credits that cross units natively at a later take
//! apply here at once, which no program can tell apart within one unit.
use super::*;
use hirsel_core_layout::cross::{self, CLIENT, SEQ};

/// Client of connection `i` is `CONN_BASE + 2i`, its inbox `CONN_BASE + 2i + 1`.
pub(super) const CONN_BASE: u64 = 0x80_0000;

pub(super) struct Conn {
    item: TypeRef,
    cap: u64,
    room: u64,
    used: u64,
    queue: VecDeque<Item>,
    /// Tickets of waiting sends, oldest first.
    waiting: VecDeque<u64>,
    tickets: u64,
    clients: u64,
    inbox: bool,
    charge: u64,
    limit: Option<Limit>,
}

pub(super) struct Item {
    bytes: Vec<u8>,
    seqs: Vec<Vec<u8>>,
    charge: u64,
}

fn conn(handle: u64) -> usize {
    ((handle - CONN_BASE) / 2) as usize
}

impl Machine<'_> {
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

    /// Drop a crossing value that nobody took: its buffers left with its charge,
    /// so only its clients remain to clean.
    fn cross_discard(&mut self, item: &Item, owners: &[[u64; 4]]) {
        for o in owners.iter().filter(|o| o[1] == CLIENT) {
            let mut word = [0u8; 8];
            word.copy_from_slice(&item.bytes[o[0] as usize..o[0] as usize + 8]);
            self.client_drop(u64::from_le_bytes(word));
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
            cap,
            room,
            used: 0,
            queue: VecDeque::new(),
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
            if let Some(front) = self.conns[i].queue.front() {
                let charge = front.charge;
                if let Err(available) = self.admit(cur, charge) {
                    return Ok(vec![2, charge, available]);
                }
                let item = self.conns[i].queue.pop_front().expect("the front item");
                let owners = self.cross_owners(self.conns[i].item);
                self.cross_put(dest, &item, &owners, cur);
                self.conns[i].used -= charge;
                self.wake(Condition::ConnSend(i));
                return Ok(vec![0, 0, 0]);
            }
            if self.conns[i].clients == 0 {
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
        if self.conns[i].clients == 0 {
            self.wake(Condition::ConnReceive(i));
        }
        self.conn_freed(i);
    }

    /// Cleaning an inbox seals its connection, settles its waiting sends
    /// `Closed`, and drops what was not received, last accepted first.
    pub(super) fn inbox_drop(&mut self, inbox: u64) {
        let i = conn(inbox);
        self.conns[i].inbox = false;
        self.wake(Condition::ConnSend(i));
        let owners = self.cross_owners(self.conns[i].item);
        while let Some(item) = self.conns[i].queue.pop_back() {
            self.conns[i].used -= item.charge;
            self.cross_discard(&item, &owners);
        }
        self.conn_freed(i);
    }

    /// The last release retires the connection by a notice to its admitting
    /// record (CONC-17), taken at a later take position even when that unit made
    /// the release: here the owner's next event, as a pending refund.
    fn conn_freed(&mut self, i: usize) {
        let c = &mut self.conns[i];
        if c.clients == 0
            && !c.inbox
            && let Some(l) = c.limit.take()
        {
            let n = c.charge;
            let owner = l.find().0.owner;
            if owner < self.sched.tasks.len() {
                self.sched.tasks[owner].refunds.push((l, n));
            } else {
                self.refund(l, n);
            }
        }
    }
}
