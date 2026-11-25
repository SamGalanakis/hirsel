//! The world half of the evaluator's engine operations and handle cleanup:
//! tasks, channels, latests, connections, host and launch state.
use super::*;

impl Machine<'_> {
    pub(super) fn rt_world(&mut self, t: &mut Task, op: RtOp, a: &[u64]) -> X<Vec<u64>> {
        let cur = t.limits.last().unwrap();
        Ok(match op {
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } => {
                self.deny_describe_capability();
                let unit = matches!(op, RtOp::SpawnUnit { .. });
                let x = TypeRef(x.0);
                let r = TypeRef(r.0);
                let n = a[0];
                let charge = charge::task(self.logical(r), n).unwrap_or(u64::MAX);
                if let Err(room) = self.admit(cur, charge) {
                    // x stays the caller's
                    return Ok(if unit {
                        vec![0, charge, room]
                    } else {
                        vec![0, room]
                    });
                }
                // CONC-15: then x's movable charge against n.
                let owners = if unit {
                    self.cross_owners(x)
                } else {
                    Vec::new()
                };
                let moved = self.cross_charge(a[1], &owners);
                if unit && moved > n {
                    self.refund(cur.clone(), charge);
                    return Ok(vec![0, moved, n]);
                }
                // The child's limit serves the child, the next scheduler record.
                let lim = Limit::new(n, (!unit).then(|| cur.clone()), self.host.sched.tasks.len());
                let xb = self.block(self.held(x), None, 0, None);
                if unit {
                    self.check_cross_sockets(a[1], &owners);
                    let item = self.cross_take(a[1], x, &owners);
                    lim.add(moved);
                    self.cross_put(xb, &item, &owners, &lim);
                } else {
                    let bytes = self.get(a[1], self.held(x) as usize).to_vec();
                    self.put(xb, &bytes);
                }
                let rb = self.block(self.held(r), None, 0, None);
                // The child's primary cell is part of the spawn's charge to `cur`.
                let inherited = t
                    .step_bounds
                    .last()
                    .map(|bound| self.step_bound(bound.left));
                let mut child = Task {
                    step_bounds: inherited.into_iter().collect(),
                    slice_steps: 0,
                    limits: vec![lim],
                    chain: Vec::new(),
                    cells: Vec::new(),
                    obligations: vec![Some(Cell {
                        limit: cur.clone(),
                        amount: charge::CAUSE_CELL,
                        resource: false,
                    })],
                    reserved: Vec::new(),
                    at: Vec::new(),
                    k: 0,
                    shield: 0,
                };
                let f = FuncId((a[2] - FN_BASE) as u32);
                let xc = self.m.types[x.index()].carried;
                let rc = self.m.types[r.index()].carried;
                let mut args = Vec::with_capacity(3);
                if matches!(rc, Carried::Address) {
                    args.push(rb)
                }
                args.push(0); // A plain callable's environment.
                match xc {
                    Carried::Address => args.push(xb),
                    Carried::Value(t) => args.push(self.load(xb, bytes_of(t))),
                    Carried::Nothing => {}
                }
                let i = self.host.children.len();
                self.host.children.push(Child {
                    status: 0,
                    result: rb,
                    r,
                    charge,
                    n,
                    limit: Some(cur.clone()),
                    joined: false,
                    unit_owner: unit.then(|| self.host.sched.tasks[t.k].unit),
                    end_taken: false,
                    crossed_result: None,
                    unit_limit: None,
                    end_kept: 0,
                    cells: Vec::new(),
                    chain: Vec::new(),
                    primary: None,
                });
                // CONC-3: the child is readied at the tail and the parent goes on.
                child.k = i + 1;
                let id = self.next_id();
                self.host.sched.tasks.push(Rec {
                    id,
                    state: State::Ready,
                    cancel: false,
                    polls: POLLS_PER_TURN,
                    cancellable: false,
                    cancelled: false,
                    start: Some((child, f, args)),
                    refunds: Vec::new(),
                    unit: if unit {
                        i + 1
                    } else {
                        self.host.sched.tasks[t.k].unit
                    },
                    notices: VecDeque::new(),
                });
                self.host.links.push(None);
                self.host.sched.ready.push_back(i + 1);
                self.event(SPAWN, id, n);
                if unit {
                    vec![TASK_BASE + i as u64, 0, 0]
                } else {
                    vec![TASK_BASE + i as u64, 0]
                }
            }
            RtOp::Join { r } => {
                let i = self.child(a[0]);
                let id = self.host.sched.tasks[i + 1].id;
                // CONC-6: a cancellation point at entry and at each wait.
                loop {
                    self.check_cancel(t)?;
                    if self.child_end_taken(i) {
                        break;
                    }
                    self.park_task(t, Condition::Join(i));
                }
                let status = self.host.children[i].status;
                let mut moved = 0;
                if status == 0 {
                    if let Some(item) = self.host.children[i].crossed_result.take() {
                        let payer = self.host.children[i].limit.as_ref().unwrap().clone();
                        self.cross_put(a[1], &item, &self.cross_owners(TypeRef(r.0)), &payer);
                        moved = item.charge;
                    } else {
                        let n = self.held(TypeRef(r.0)) as usize;
                        let bytes = self.get(self.host.children[i].result, n).to_vec();
                        self.put(a[1], &bytes);
                    }
                }
                self.event(JOIN, id, u64::from(status != 0));
                let cells = self.settle_child(i, moved);
                if status == 0 {
                    vec![0, 0]
                } else {
                    let chain = std::mem::take(&mut self.host.children[i].chain);
                    vec![u64::from(status), self.deliver(chain, cells)]
                }
            }
            RtOp::Cancel => {
                let i = self.child(a[0]);
                self.cancel(i);
                vec![]
            }
            RtOp::CheckCancel => {
                self.check_cancel(t)?;
                self.event(CHECK_CANCEL, 0, 0);
                vec![]
            }
            RtOp::ArgsLen => vec![self.host.world.args.len() as u64],
            RtOp::ArgsGet => vec![
                self.host
                    .world
                    .args
                    .get(a[1] as usize)
                    .copied()
                    .unwrap_or(0),
            ],
            RtOp::LaunchWord => vec![self.host.world.launch.as_ref().unwrap().word(a[0], a[1])],
            RtOp::LaunchHandle => vec![0],
            RtOp::SecretShare => vec![self.secret_share(a[0])],
            RtOp::DescribeEmit => {
                if self.host.world.launch.as_ref().unwrap().is_describe() {
                    let (bytes, _, _, _) = self.carrier(a[0]);
                    self.host.world.stdout.extend_from_slice(&bytes);
                    vec![1]
                } else {
                    vec![0]
                }
            }
            RtOp::LaunchTarget { .. } => {
                let (data, len) = self.host.world.launch_target;
                self.store(a[0], 8, data);
                self.store(a[0] + 8, 8, len);
                self.store(a[0] + 16, 8, 0);
                vec![]
            }
            RtOp::Host(op) => self.host(t, op, a)?,
            RtOp::Flow(_) => panic!("flow execution uses the native reference"),
            RtOp::ChannelNew { item } => {
                let item = TypeRef(item.0);
                if a[0] == 0 {
                    return Err(Stop::Cause("Bounds"));
                }
                let charge = charge::channel(a[0], self.logical(item)).unwrap_or(u64::MAX);
                match self.admit(cur, charge) {
                    Err(room) => vec![0, 0, room],
                    Ok(()) => {
                        let id = self.next_id();
                        self.event(SENDERS, id, 1);
                        self.host.chans.push(Chan {
                            id,
                            item,
                            cap: a[0],
                            queue: VecDeque::new(),
                            senders: 1,
                            receiver: End::Open,
                            charge,
                            limit: Some(cur.clone()),
                        });
                        let s = CHAN_BASE + 2 * (self.host.chans.len() as u64 - 1);
                        vec![s, s + 1, 0]
                    }
                }
            }
            RtOp::ConnNew { item } => self.conn_new(&cur.clone(), item, a[0], a[1])?,
            RtOp::ConnClient => self.conn_client(a[0]),
            RtOp::ConnSend { .. } | RtOp::ConnTrySend { .. } => {
                let wait = matches!(op, RtOp::ConnSend { .. });
                self.conn_send(t, wait, a[0], a[1])?
            }
            RtOp::ConnReceive { .. } | RtOp::ConnTryReceive { .. } => {
                let wait = matches!(op, RtOp::ConnReceive { .. });
                let cur = cur.clone();
                self.conn_receive(t, &cur, wait, a[0], a[1])?
            }
            RtOp::ChannelSender => {
                let c = &mut self.host.chans[((a[0] - CHAN_BASE) / 2) as usize];
                c.senders += 1;
                let (id, senders) = (c.id, c.senders);
                self.event(SENDERS, id, senders);
                vec![a[0]]
            }
            RtOp::Send { item } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                // CONC-5: a cancellation point at entry and at each wait.
                loop {
                    self.check_cancel(t)?;
                    match self.publish(i, a[1], item) {
                        0 => break vec![1],
                        2 => {
                            self.event(
                                PUBLISH,
                                self.chan_id(i),
                                hirsel_core_layout::event::CHANNEL_CLOSED,
                            );
                            break vec![0];
                        }
                        _ => self.park_task(t, Condition::Send(i)),
                    }
                }
            }
            RtOp::TrySend { item } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                let status = self.publish(i, a[1], item);
                let source = self.site(t);
                // The send's own event: the turn it may give up can record more after it.
                let event = self.host.sched.events.as_ref().map(Vec::len);
                self.event(TRY_SEND, self.chan_id(i), status);
                if let (Some(event), Some(events)) = (event, &mut self.host.sched.events) {
                    events[event].source = source;
                }
                vec![status]
            }
            RtOp::Receive { .. } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                loop {
                    self.check_cancel(t)?;
                    if self.take(i, a[1]) {
                        break vec![1];
                    }
                    if self.host.chans[i].senders == 0 {
                        self.event(
                            RECEIVE,
                            self.chan_id(i),
                            hirsel_core_layout::event::CHANNEL_CLOSED,
                        );
                        break vec![0];
                    }
                    self.park_task(t, Condition::Receive(i));
                }
            }
            RtOp::LatestNew { item } => match self.admit(cur, charge::LATEST_ROOT) {
                Err(room) => vec![0, room],
                Ok(()) => {
                    let id = self.next_id();
                    self.event(SENDERS, id, 1);
                    self.host.latests.push(Latest {
                        id,
                        item: TypeRef(item.0),
                        root: a[0],
                        handles: 1,
                        limit: Some(cur.clone()),
                    });
                    vec![LATEST_BASE + self.host.latests.len() as u64 - 1, 0]
                }
            },
            RtOp::LatestHandle => {
                let l = &mut self.host.latests[(a[0] - LATEST_BASE) as usize];
                l.handles += 1;
                let (id, handles) = (l.id, l.handles);
                self.event(SENDERS, id, handles);
                vec![a[0]]
            }
            RtOp::LatestPublish => {
                let l = &mut self.host.latests[(a[0] - LATEST_BASE) as usize];
                let (id, item, old) = (l.id, l.item, std::mem::replace(&mut l.root, a[1]));
                self.event(LATEST_PUBLISH, id, 0);
                self.release_latest_holder(t, old, item);
                vec![]
            }
            RtOp::LatestRead => {
                let l = &self.host.latests[(a[0] - LATEST_BASE) as usize];
                let (id, root) = (l.id, l.root);
                self.event(LATEST_READ, id, 0);
                self.share_counted(root, Counted::Arc);
                vec![root]
            }
            RtOp::TryReceive { .. } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                let status = if self.take(i, a[1]) {
                    0
                } else if self.host.chans[i].senders == 0 {
                    2
                } else {
                    1
                };
                self.event(TRY_RECEIVE, self.chan_id(i), status);
                vec![status]
            }
            _ => unreachable!("{op:?} is a core engine operation"),
        })
    }

    pub(super) fn drop_world_handle(&mut self, t: &mut Task, at: u64, h: Handle) -> bool {
        match h {
            Handle::Flow => panic!("flow execution uses the native reference"),
            Handle::Socket => {
                self.socket_drop(self.load(at, 8));
                false
            }
            Handle::Media => {
                self.drop_media(self.load(at, 8));
                false
            }
            Handle::Secret => {
                self.secret_drop(self.load(at, 8));
                false
            }
            // The provider closes the file and ignores the outcome (core.md,
            // MEM-24); the trace records the close by its handle.
            Handle::HostFile => {
                let h = self.load(at, 8);
                if h & LISTING_HANDLE != 0 {
                    self.host.world.listings[(h & !LISTING_HANDLE) as usize] = None;
                    return false;
                }
                if (SCRAM_BASE..PORT_HANDLE).contains(&h) {
                    let (_, payer) = self.host.world.scrams.remove(&h).unwrap();
                    self.refund(payer, 32);
                    return false;
                }
                self.trace.push(h);
                if (PORT_HANDLE..CALL_BASE).contains(&h) {
                    // A served Dir's file or listing: the server learns by
                    // Released (FAIL-13).
                    self.port_release(h);
                    return false;
                }
                if h >= CALL_BASE || (SERVED_BASE..SERVED_BASE + 0x10_0000).contains(&h) {
                    // A dropped Call settles its caller as sealed; a cleaned
                    // Served end seals its port.
                    self.port_drop(h);
                    return false;
                }
                if (CELL_BASE..LISTING_HANDLE).contains(&h) {
                    // A dropped ClockHand freezes its clock; a dropped
                    // StopTrigger never stops its process.
                    return false;
                }
                if let Some(file) = h
                    .checked_sub(FILE_BASE)
                    .and_then(|i| self.host.world.files.get_mut(i as usize))
                {
                    *file = None;
                }
                false
            }
            Handle::Task => {
                // CONC-8: cancel the child and wait for its end with cancellation
                // shielded, then clean its result or chain (native `abandon`).
                let i = self.child(self.load(at, 8));
                let id = self.host.sched.tasks[i + 1].id;
                t.shield += 1;
                self.cancel(i);
                while !self.child_end_taken(i) {
                    self.park_task(t, Condition::Join(i));
                }
                self.event(JOIN, id, 2);
                let (status, result, r) = {
                    let c = &self.host.children[i];
                    (c.status, c.result, c.r)
                };
                // CONC-8: an abandoned handle cleans the result or the chain.
                let failed = if let Some(item) = self.host.children[i].crossed_result.take() {
                    self.cross_discard(&item, &self.cross_owners(r));
                    false
                } else {
                    status == 0 && self.drop_value(t, result, r)
                };
                for c in std::mem::take(&mut self.host.children[i].chain) {
                    if c.message[2] != 0 {
                        self.free(c.message[0]);
                    }
                }
                for (l, n) in self.settle_child(i, 0) {
                    self.refund(l, n);
                }
                t.shield -= 1;
                failed
            }
            Handle::Sender => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.host.chans[i].senders -= 1;
                let (id, senders) = (self.host.chans[i].id, self.host.chans[i].senders);
                self.event(SENDERS, id, senders);
                if senders == 0 {
                    self.event(CLOSE, id, 0);
                    self.wake(Condition::Receive(i));
                }
                self.chan_freed(i);
                false
            }
            Handle::Client => {
                self.client_drop(self.load(at, 8));
                false
            }
            Handle::Inbox => {
                t.shield += 1;
                self.inbox_drop(self.load(at, 8));
                t.shield -= 1;
                false
            }
            Handle::Latest => {
                let i = (self.load(at, 8) - LATEST_BASE) as usize;
                self.host.latests[i].handles -= 1;
                let (id, handles) = (self.host.latests[i].id, self.host.latests[i].handles);
                self.event(SENDERS, id, handles);
                if self.host.latests[i].handles != 0 {
                    return false;
                }
                let latest = &mut self.host.latests[i];
                let (item, root, limit) = (latest.item, latest.root, latest.limit.take().unwrap());
                self.release_latest_holder(t, root, item);
                self.refund(limit, charge::LATEST_ROOT);
                false
            }
            Handle::Receiver => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.host.chans[i].receiver = End::Cleaning;
                t.shield += 1;
                self.event(CLOSE, self.host.chans[i].id, 1);
                self.wake(Condition::Send(i));
                let mut failed = false;
                // CONC-9: last published first; a failure appends and the rest go on.
                while let Some(bytes) = self.host.chans[i].queue.pop_back() {
                    let tmp = self.block(bytes.len() as u64, None, 0, None);
                    self.put(tmp, &bytes);
                    failed |= self.drop_value(t, tmp, self.host.chans[i].item);
                }
                self.host.chans[i].receiver = End::Gone;
                self.chan_freed(i);
                t.shield -= 1;
                failed
            }
            _ => unreachable!("{h:?} is a core handle"),
        }
    }

    pub(super) fn socket_drop(&mut self, key: u64) {
        if let Some(payer) = self.host.world.exec_handles.remove(&key) {
            self.refund(payer, charge::EXEC_HANDLE);
        }
        if key & 3 == 2 {
            self.host.world.tls_writes.remove(&(key & !3));
        }
        if !self.port_socket_part(key, false) {
            self.host.world.network.sockets.release(key);
        }
    }

    pub(super) fn child_end_taken(&mut self, child: usize) -> bool {
        if self.host.children[child].unit_owner.is_none() {
            return self.host.sched.tasks[child + 1].state == State::Ended;
        }
        if !self.host.children[child].end_taken {
            self.take_unit_notices();
        }
        self.host.children[child].end_taken
    }

    /// Refund a joined or abandoned task's record, result slot and unused primary
    /// cell; the reserved `n` came back when the child's limit ended. The spawn's
    /// charge holds the child's first primary cell, which may have left with a
    /// chain: a used cell is among the cells returned to go with its `Causes`, or
    /// went with one the child caught. A helper's end take returned all but
    /// `size(R)`, the result's content and what it left behind; the join
    /// returns the first two less the content `moved` into the result.
    pub(super) fn settle_child(&mut self, i: usize, moved: u64) -> Vec<(Limit, u64)> {
        let c = &mut self.host.children[i];
        assert!(!c.joined, "task handle used twice");
        c.joined = true;
        let n = if c.unit_owner.is_some() {
            c.end_kept - moved
        } else {
            c.charge - c.n - charge::CAUSE_CELL
        };
        let l = c.limit.take().unwrap();
        let cells = std::mem::take(&mut c.cells);
        let primary = c.primary.take();
        self.refund(l, n);
        if let Some(p) = primary {
            self.refund(p.limit, p.amount);
        }
        cells
    }

    pub(super) fn release_latest_holder(&mut self, t: &mut Task, node: u64, item: TypeRef) {
        let place = self.block(8, None, 0, None);
        self.put(place, &node.to_le_bytes());
        assert!(
            !self.drop_value(t, place, item),
            "a shareable root's release failed"
        );
    }
}

impl<'m> Host<'m> for WorldHost<'m> {
    fn current(&self) -> usize {
        self.sched.current
    }
    fn event(m: &mut Machine<'m, Self>, kind: u8, object: u64, value: u64) {
        m.world_event(kind, object, value);
    }
    fn drain_refunds(m: &mut Machine<'m, Self>) {
        let k = m.host.sched.current;
        if k < m.host.sched.tasks.len() && !m.host.sched.tasks[k].refunds.is_empty() {
            for (l, n) in std::mem::take(&mut m.host.sched.tasks[k].refunds) {
                m.refund(l, n);
            }
        }
    }
    fn defer_refund(m: &mut Machine<'m, Self>, l: &Limit, n: u64) -> bool {
        let owner = l.0.owner;
        if owner == m.host.sched.current || owner >= m.host.sched.tasks.len() {
            return false;
        }
        if n != 0 {
            m.event(REFUND, m.host.sched.tasks[owner].id, n);
        }
        m.host.sched.tasks[owner].refunds.push((l.clone(), n));
        true
    }
    fn hand_over(m: &mut Machine<'m, Self>, l: &Limit, parent: &Limit) {
        m.drain_refunds();
        let unused = l.0.cap - l.used();
        let (owner, current) = (parent.0.owner, m.host.sched.current);
        if owner < m.host.sched.tasks.len() {
            m.event(REFUND, m.host.sched.tasks[owner].id, unused);
        }
        if owner == current || owner >= m.host.sched.tasks.len() {
            parent.subtract(unused);
        } else {
            m.host.sched.tasks[owner]
                .refunds
                .push((parent.clone(), unused));
        }
    }
    fn slice_end(m: &mut Machine<'m, Self>) {
        m.event(hirsel_core_layout::event::STEP_SLICE, 0, 65_536);
        m.park_turn(None, false);
    }
    fn enter_call(m: &mut Machine<'m, Self>, t: &Task, frame: usize) {
        if m.host.sched.current != 0 {
            let used = TASK_STACK_ROOM.with(|room| room.get().0.abs_diff(frame));
            m.host.task_stack_peak = m.host.task_stack_peak.max(used);
            m.host.task_call_depth = m.host.task_call_depth.max(t.at.len() + 1);
        }
    }
    fn rt(m: &mut Machine<'m, Self>, t: &mut Task, op: RtOp, a: &[u64]) -> X<Vec<u64>> {
        m.rt_world(t, op, a)
    }
    fn drop_handle(m: &mut Machine<'m, Self>, t: &mut Task, at: u64, h: Handle) -> bool {
        m.drop_world_handle(t, at, h)
    }
}
