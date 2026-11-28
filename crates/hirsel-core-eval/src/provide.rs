use super::*;

impl VirtualCell {
    pub(super) fn port(
        &mut self,
    ) -> (
        &mut BTreeMap<u64, PortCall>,
        &mut BTreeMap<u64, PortHandle>,
        &mut u64,
        &mut u64,
        Vocab,
    ) {
        let VirtualCell::Port {
            calls,
            handles,
            used,
            next,
            vocab,
            ..
        } = self
        else {
            unreachable!("a port")
        };
        (calls, handles, used, next, *vocab)
    }
    /// The oldest queued call or Released notice.
    pub(super) fn queued(&self) -> Option<Queued> {
        let VirtualCell::Port { calls, handles, .. } = self else {
            unreachable!("a port")
        };
        let call = calls
            .iter()
            .find(|(_, c)| !c.taken && !c.finished && c.answer.is_none())
            .map(|(&id, _)| id);
        let released = handles
            .iter()
            .flat_map(|(&h, r)| {
                let read = match r.state {
                    PortState::Released(at) => Some((at, h, false)),
                    _ => None,
                };
                let write = match r.write {
                    Some(PortState::Released(at)) => Some((at, h, true)),
                    _ => None,
                };
                read.into_iter().chain(write)
            })
            .min();
        match (call, released) {
            (Some(c), Some((at, h, w))) if at < c => Some(Queued::Released(h, w)),
            (Some(c), _) => Some(Queued::Call(c)),
            (None, Some((_, h, w))) => Some(Queued::Released(h, w)),
            (None, None) => None,
        }
    }
    pub(super) fn free_staged(&mut self, id: u64) {
        let (calls, _, used, _, _) = self.port();
        if let Some(call) = calls.get_mut(&id) {
            *used -= call.staged.len() as u64;
            call.staged = Vec::new();
        }
    }
    /// A Close call ends: a server that took it knows the file is gone; one
    /// that never saw it learns by `Released`.
    pub(super) fn closed_call(&mut self, id: u64) {
        let (calls, handles, used, next, vocab) = self.port();
        let Some(call) = calls.get(&id) else {
            return;
        };
        if vocab != Vocab::Dir || call.words[0] != DirOp::Close.word() {
            return;
        }
        let file = call.words[1];
        if call.taken {
            if handles.remove(&file).is_some() {
                *used -= HANDLE_ROOM;
            }
        } else if let Some(h) = handles.get_mut(&file) {
            h.state = PortState::Released(*next);
            *next += 1;
        }
    }
    /// Settle an unsettled call; returns its caller.
    pub(super) fn settle(&mut self, id: u64, answer: Vec<u64>) -> Option<usize> {
        let (calls, ..) = self.port();
        let call = calls.get_mut(&id)?;
        if call.answer.is_some() || call.finished {
            return None;
        }
        call.answer = Some(answer);
        let caller = call.caller;
        self.free_staged(id);
        self.closed_call(id);
        Some(caller)
    }
    pub(super) fn prune(&mut self, id: u64) {
        let (calls, ..) = self.port();
        if calls.get(&id).is_some_and(|c| c.finished && !c.held) {
            self.free_staged(id);
            self.port().0.remove(&id);
        }
    }
}

impl Machine<'_> {
    pub(super) fn read_call_payload(
        &mut self,
        key: u64,
        index: u64,
        max: u64,
        out: u64,
    ) -> X<Vec<u64>> {
        let (port, id) = Self::call_of(key);
        let VirtualCell::Port { calls, .. } = &self.host.world.cells[port] else {
            unreachable!("a port");
        };
        let call = &calls[&id];
        if call.finished || call.answer.is_some() {
            return Ok(vec![0; 4]);
        }
        let field = call
            .fields
            .get(index as usize)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let field_len = field.len() as u64;
        let count = field_len.min(max);
        let len = self.load(out + 8, 8);
        let cap = self.load(out + 16, 8);
        if cap.saturating_sub(len) < count {
            return Err(Stop::Cause("Bounds"));
        }
        if count != 0 {
            let bytes = field[..count as usize].to_vec();
            let data = self.load(out, 8);
            self.put(data + len, &bytes);
            self.store(out + 8, 8, len + count);
        }
        Ok(vec![field_len - count, 0, 0, count])
    }

    pub(super) fn dir_call(
        &mut self,
        t: &Task,
        op: HostOp,
        c: usize,
        h: u64,
        a: &[u64],
    ) -> X<Vec<u64>> {
        self.check_cancel(t).inspect_err(|_| {
            // A close cancelled before issue still ends the file: the server
            // learns by Released (native `host_file` CLOSE under a shield).
            if op == HostOp::Close {
                self.port_release(a[0]);
            }
        })?;
        if matches!(op, HostOp::Write | HostOp::WriteAt | HostOp::ZeroAt)
            && let VirtualCell::Port { handles, .. } = &self.host.world.cells[c]
            && handles
                .get(&h)
                .is_some_and(|file| file.read_only_file && file.state == PortState::Live)
        {
            return Ok(vec![0, DENIED, 0, 0]);
        }
        let spare = |m: &Self, carrier: u64| {
            let (_, _, len, cap) = m.carrier(carrier);
            cap.saturating_sub(len)
        };
        // The caller-side checks of every provider's entry (native `drive`).
        match op {
            HostOp::Mkdtemp | HostOp::Mkstemp if spare(self, a[1]) < 32 => {
                return Err(Stop::Cause("Bounds"));
            }
            HostOp::NextEntries if spare(self, a[1]) < 258 => return Err(Stop::Cause("Bounds")),
            HostOp::WriteAt
                if a[1]
                    .checked_add(self.carrier_bytes(a[2]).len() as u64)
                    .is_none() =>
            {
                return Err(Stop::Cause("Bounds"));
            }
            _ => {}
        }
        if matches!(op, HostOp::WriteAt | HostOp::ZeroAt)
            && let VirtualCell::Port { handles, .. } = &self.host.world.cells[c]
            && let Some(handle) = handles.get(&h)
        {
            if op == HostOp::ZeroAt {
                if !matches!(handle.file_mode, 1 | 3 | 4 | MODE_DURABLE | MODE_DIRECT) {
                    return Ok(vec![0, DENIED, 0, 0]);
                }
                if let Some((tag, code)) = zero_at_error(handle.file_mode, a[1], a[2]) {
                    return Ok(vec![0, tag, code, 0]);
                }
            } else if handle.file_mode == MODE_DIRECT {
                let (_, data, len, _) = self.carrier(a[2]);
                if !a[1].is_multiple_of(DIRECT_ALIGNMENT)
                    || !len.is_multiple_of(DIRECT_ALIGNMENT)
                    || !self.direct_buffer_aligned(data)
                {
                    return Ok(vec![0, OTHER, 22, 0]);
                }
            }
        }
        let path = |m: &Self, i: usize| m.carrier_bytes(a[i]).to_vec();
        let dir_op = DirOp::from_host(op.code()).expect("a Dir, file or listing operation");
        let (kind, fields, payload, out): (u8, [u64; 3], Vec<Vec<u8>>, Option<u64>) = match dir_op {
            DirOp::Open => {
                let p = path(self, 1);
                (0, [h, p.len() as u64, a[2]], vec![vec![], p], None)
            }
            DirOp::OpenDir | DirOp::Stat | DirOp::Mkdir | DirOp::Remove | DirOp::RemoveTree => {
                let p = path(self, 1);
                (0, [h, p.len() as u64, 0], vec![vec![], p], None)
            }
            DirOp::Mkdtemp | DirOp::Mkstemp => {
                let (prefix, ..) = self.carrier(a[1]);
                let room = spare(self, a[1]);
                (
                    0,
                    [h, prefix.len() as u64, room],
                    vec![vec![], prefix],
                    Some(a[1]),
                )
            }
            DirOp::Rename => {
                let (from, to) = (path(self, 1), path(self, 2));
                (
                    0,
                    [h, from.len() as u64, to.len() as u64],
                    vec![vec![], from, to],
                    None,
                )
            }
            DirOp::SyncDir | DirOp::ZeroExtension => (0, [h, 0, 0], vec![], None),
            DirOp::Entries => {
                let p = if a[1] == 0 { vec![] } else { path(self, 1) };
                (0, [h, p.len() as u64, 0], vec![vec![], p], None)
            }
            DirOp::Read => (1, [h, spare(self, a[1]), 0], vec![], Some(a[1])),
            DirOp::ReadAt => (1, [h, a[1], spare(self, a[2])], vec![], Some(a[2])),
            DirOp::Write => {
                let (b, ..) = self.carrier(a[1]);
                (1, [h, b.len() as u64, 0], vec![vec![], b], None)
            }
            DirOp::WriteAt => {
                let (b, ..) = self.carrier(a[2]);
                (1, [h, a[1], b.len() as u64], vec![vec![], vec![], b], None)
            }
            DirOp::ZeroAt => (1, [h, a[1], a[2]], vec![], None),
            DirOp::StatFile
            | DirOp::Sync
            | DirOp::TryLock
            | DirOp::TryLockShared
            | DirOp::Unlock
            | DirOp::Close => (1, [h, 0, 0], vec![], None),
            DirOp::NextEntries => (2, [h, spare(self, a[1]), 0], vec![], Some(a[1])),
            DirOp::Released => unreachable!("a notice is queued at a drop"),
        };
        let VirtualCell::Port { handles, .. } = &self.host.world.cells[c] else {
            unreachable!()
        };
        let live = handles.get(&h).is_some_and(|handle| {
            handle.kind == kind && handle.state == PortState::Live && handle.write.is_none()
        });
        let mut w = [0; 9];
        w[0] = dir_op.word();
        w[1..4].copy_from_slice(&fields);
        let mut answer = self.port_call(t, c, w, payload, out, live)?;
        if answer[1] == 0 {
            let narrow = a[0] & DIR_READ_ONLY != 0;
            if op == HostOp::Open {
                let (_, file) = self.port_handle(answer[0]).expect("a registered file");
                let (_, handles, ..) = self.host.world.cells[c].port();
                let handle = handles.get_mut(&file).unwrap();
                handle.read_only_file = narrow;
                handle.file_mode = a[2] as u8;
            } else if narrow && matches!(op, HostOp::OpenDir | HostOp::Mkdtemp) {
                answer[0] |= DIR_READ_ONLY;
            }
        }
        Ok(answer)
    }
}

impl Machine<'_> {
    pub(super) fn provide_random_tape(&mut self, task: &Task, source: u64) -> Vec<u64> {
        let charge = hirsel_core_layout::charge::PROVIDE_CELL + self.load(source + 8, 8);
        let payer = task.limits.last().unwrap();
        if let Err(available) = self.admit(payer, charge) {
            return vec![charge, 1, 0, available];
        }
        let tape = RandomStream::tape(self.carrier_bytes(source).to_vec());
        let cell = self.host.world.cells.len();
        self.host.world.cells.push(VirtualCell::Random(tape));
        let held = (payer.clone(), charge);
        self.host.world.cell_charges.insert(cell, held);
        vec![CELL_BASE + cell as u64, 0, 0, 0]
    }

    /// MEM-24: the limit `l` ends; withdraw the cells charged to it. Their
    /// waits are readied: a deadline passes, a stop and an idle wake, and the
    /// operation then fails with Bounds, as every later one on the cell does.
    pub(super) fn withdraw_cells(&mut self, l: &Limit) {
        let world = &mut self.host.world;
        let ending: Vec<usize> = world
            .cell_charges
            .iter()
            .filter(|(_, (payer, _))| Arc::ptr_eq(&payer.0, &l.0))
            .map(|(&cell, _)| cell)
            .collect();
        for cell in ending {
            let (payer, charge) = self.host.world.cell_charges.remove(&cell).unwrap();
            self.host.world.withdrawn.insert(cell);
            let mut ready = Vec::new();
            let mut receivers = Vec::new();
            match &mut self.host.world.cells[cell] {
                VirtualCell::Clock {
                    mono,
                    boot,
                    wall,
                    timers,
                    wall_timers,
                    idle,
                } => {
                    (*mono, *boot, *wall) = (u64::MAX, u64::MAX, u64::MAX);
                    receivers.extend(timers.drain(..).map(|(_, k)| k));
                    ready.extend(wall_timers.drain(..).map(|(_, k)| k));
                    idle.iter_mut().for_each(|(_, woken)| *woken = true);
                    ready.extend(idle.iter().map(|&(k, _)| k));
                }
                VirtualCell::Random(stream) => *stream = RandomStream::tape(Vec::new()),
                VirtualCell::Process { stopped, waiters } => {
                    *stopped = true;
                    ready.append(waiters);
                }
                VirtualCell::Port { .. } => unreachable!("a port keeps its run lifetime"),
            }
            for k in receivers {
                if matches!(
                    self.host.sched.tasks[k].state,
                    State::Waiting(c) if c.is_receive()
                ) {
                    self.host.sched.wake_waiting(k);
                } else {
                    self.ready_provider(k);
                }
            }
            ready.into_iter().for_each(|k| self.ready_provider(k));
            self.refund(payer, charge);
        }
    }

    pub(super) fn draw_virtual_random(&mut self, cell: usize, carrier: u64) -> X<Vec<u64>> {
        let (_, data, len, cap) = self.carrier(carrier);
        let spare = cap.saturating_sub(len) as usize;
        if spare == 0 {
            return Ok(vec![]);
        }
        let (block, offset) = self.check_write(data + len, spare);
        let VirtualCell::Random(stream) = &mut self.host.world.cells[cell] else {
            unreachable!()
        };
        let bytes = &mut self.mem[block].bytes[offset..offset + spare];
        // SAFETY: MaybeUninit<u8> has the layout of u8 and draw only writes initialized bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), spare) };
        if !stream.draw(out) {
            return Err(Stop::Cause("Bounds"));
        }
        self.store(carrier + 8, 8, len + spare as u64);
        Ok(vec![])
    }
    /// The `provide` rows, and Clock, Random and Process operations on a cell.
    pub(super) fn provide(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Option<X<Vec<u64>>> {
        let ok = |value: u64, written: u64| vec![value, 0, 0, written];
        if matches!(
            op,
            HostOp::ProvideClock | HostOp::ProvideRandom | HostOp::ProvideProcess
        ) {
            // A cell holds PROVIDE_CELL until its limit ends (MEM-24); a
            // refusal answers 0 with the available bytes.
            let payer = t.limits.last().unwrap();
            let charge = hirsel_core_layout::charge::PROVIDE_CELL;
            if let Err(available) = self.admit(payer, charge) {
                return Some(Ok(ok(0, available)));
            }
            let held = (payer.clone(), charge);
            self.host
                .world
                .cell_charges
                .insert(self.host.world.cells.len(), held);
        }
        // An operation on a withdrawn cell fails with Bounds at issue, and so
        // does a wait its withdrawal readied.
        let on_cell = matches!(
            op,
            HostOp::ClockAdvance
                | HostOp::ClockAdvanceBoot
                | HostOp::ClockSetWall
                | HostOp::ProcessTrigger
                | HostOp::ClockIdle
                | HostOp::WallMs
                | HostOp::MonoNs
                | HostOp::BootNs
                | HostOp::SleepUntil
                | HostOp::SleepUntilWall
                | HostOp::Random
                | HostOp::Stopped
        );
        let withdrawn = |m: &Self| {
            on_cell
                && m.cell(a[0])
                    .is_some_and(|c| m.host.world.withdrawn.contains(&c))
        };
        if withdrawn(self) {
            return Some(Err(Stop::Cause("Bounds")));
        }
        if matches!(
            op,
            HostOp::ProvideStdout
                | HostOp::ProvideStderr
                | HostOp::ProvideDir
                | HostOp::ProvideNet
                | HostOp::ProvideExec
        ) {
            // A port's room with its own record, held until the run ends.
            let payer = t.limits.last().unwrap();
            let charge = hirsel_core_layout::charge::PROVIDE_CELL.saturating_add(a[0]);
            if let Err(available) = self.admit(payer, charge) {
                return Some(Ok(ok(0, available)));
            }
            self.host.world.grant_charges.push((payer.clone(), charge));
        }
        let new = |w: &mut World, cell| {
            w.cells.push(cell);
            CELL_BASE + w.cells.len() as u64 - 1
        };
        let reply = match op {
            HostOp::ProvideClock => {
                let h = new(
                    &mut self.host.world,
                    VirtualCell::Clock {
                        mono: a[0],
                        boot: a[0],
                        wall: a[1],
                        timers: Vec::new(),
                        wall_timers: Vec::new(),
                        idle: Vec::new(),
                    },
                );
                ok(h, h)
            }
            HostOp::ProvideRandom => ok(
                new(
                    &mut self.host.world,
                    VirtualCell::Random(RandomStream::Seeded(a[0])),
                ),
                0,
            ),
            HostOp::ProvideRandomTape => self.provide_random_tape(t, a[0]),
            HostOp::ProvideProcess => {
                let process = VirtualCell::Process {
                    stopped: false,
                    waiters: Vec::new(),
                };
                let h = new(&mut self.host.world, process);
                ok(h, h)
            }
            HostOp::ClockAdvance | HostOp::ClockAdvanceBoot => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Clock {
                    mono, boot, timers, ..
                } = &mut self.host.world.cells[c]
                else {
                    unreachable!()
                };
                let before = *mono;
                *mono = (*mono).max(a[1]);
                *boot = if op == HostOp::ClockAdvanceBoot {
                    (*boot).max(a[2]).max(*mono)
                } else {
                    boot.saturating_add(*mono - before)
                };
                let now = *mono;
                let mut due: Vec<_> = timers
                    .iter()
                    .copied()
                    .filter(|&(at, _)| at <= now)
                    .collect();
                timers.retain(|&(at, _)| at > now);
                due.sort_by_key(|&(at, _)| at);
                for (_, k) in due {
                    if matches!(
                        self.host.sched.tasks[k].state,
                        State::Waiting(c) if c.is_receive()
                    ) {
                        self.host.sched.wake_waiting(k);
                    } else {
                        self.ready_provider(k);
                    }
                }
                ok(0, 0)
            }
            HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec => {
                let vocab = match op {
                    HostOp::ProvideDir => Vocab::Dir,
                    HostOp::ProvideNet => Vocab::Net,
                    HostOp::ProvideExec => Vocab::Exec,
                    _ => Vocab::Stream,
                };
                let port = VirtualCell::Port {
                    vocab,
                    size: if vocab == Vocab::Stream { a[1] } else { 0 },
                    sealed: false,
                    next: 0,
                    calls: BTreeMap::new(),
                    servers: Vec::new(),
                    handles: if vocab == Vocab::Dir {
                        BTreeMap::from([(0, PortHandle::new(0))])
                    } else {
                        BTreeMap::new()
                    },
                    room: a[0],
                    used: 0,
                };
                let h = new(&mut self.host.world, port);
                let served = h - CELL_BASE + SERVED_BASE;
                if vocab == Vocab::Exec {
                    let key = EXEC_GRANT + self.host.world.exec_grants.len() as u64;
                    self.host.world.exec_grants.push((
                        (h - CELL_BASE) as usize,
                        hirsel_core_net::exec::ExecRules::universal(),
                    ));
                    return Some(Ok(ok(key, served)));
                }
                if vocab != Vocab::Net {
                    return Some(Ok(ok(h, served)));
                }
                // A served Net is an ordinary grant of every address, handshake
                // and name (native `provide_port`), so `net.narrow`
                // intersects it as any Net's.
                let tls = hirsel_core_net::GrantRule::Tls(hirsel_core_net::TlsRule::served());
                let resolve =
                    hirsel_core_net::GrantRule::Resolve(hirsel_core_net::ResolveRule::served());
                let rules = hirsel_core_net::GrantRule::served_addresses()
                    .chain([tls, resolve])
                    .chain(hirsel_core_net::UnixDirection::ALL.into_iter().map(|dir| {
                        hirsel_core_net::GrantRule::Unix(hirsel_core_net::UnixRule::served(dir))
                    }))
                    .collect();
                let grants = &mut self.host.world.network.grants;
                let key = grants
                    .last_key_value()
                    .map_or(0, |(&key, _)| key)
                    .checked_add(1)
                    .expect("run-local handle counter");
                grants.insert(key, rules);
                self.host
                    .world
                    .net_ports
                    .insert(key, (h - CELL_BASE) as usize);
                ok(key, served)
            }
            HostOp::StdinTerminal | HostOp::StdoutTerminal | HostOp::StderrTerminal => {
                let terminal = match self.cell(a[0]) {
                    Some(c) => match self.host.world.cells[c] {
                        VirtualCell::Port { size, .. } => size != 0,
                        _ => unreachable!(),
                    },
                    None => self.host.world.terminals[usize::from(op.terminal_stream().unwrap())],
                };
                ok(u64::from(terminal), 0)
            }
            // A port answers its own size; the model world's host streams give none.
            HostOp::StdoutSize | HostOp::StderrSize => match self.cell(a[0]) {
                Some(c) => match self.host.world.cells[c] {
                    VirtualCell::Port { size, .. } => ok(size, 0),
                    _ => unreachable!(),
                },
                None => ok(0, 0),
            },
            HostOp::PortNext => {
                let c = (a[0] - SERVED_BASE) as usize;
                return Some(self.port_next(t, c));
            }
            HostOp::CallWord => {
                let (c, id) = Self::call_of(a[0]);
                let (calls, ..) = self.host.world.cells[c].port();
                ok(calls[&id].words.get(a[1] as usize).copied().unwrap_or(0), 0)
            }
            HostOp::CallSecret => {
                // A new designation charged as `secret.narrow`; any call but
                // a SendSecret fails the server (native `call_secret`).
                let (c, id) = Self::call_of(a[0]);
                let (calls, ..) = self.host.world.cells[c].port();
                let Some(secret) = calls[&id]
                    .secret
                    .as_ref()
                    .and_then(|secrets| secrets.get(a[1] as usize).cloned())
                else {
                    return Some(Err(Stop::Cause("Fail")));
                };
                let size = SecretGrant::logical_size(secret.policy.send(), 0);
                let charge = size.unwrap_or(u64::MAX);
                let payer = t.limits.last().unwrap();
                if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
                    return Some(Ok(vec![charge, 2, 0, available]));
                }
                let key = self.designate(secret, (payer.clone(), charge));
                ok(key, 0)
            }
            HostOp::CallPayload => return Some(self.read_call_payload(a[0], a[1], u64::MAX, a[2])),
            HostOp::CallPayloadPrefix => {
                return Some(self.read_call_payload(a[0], a[1], a[2], a[3]));
            }
            HostOp::PortFill => {
                // Stage reply bytes in the room, at most the call's offer; a
                // room too short refuses with error tag 1 (MEM-21).
                let (c, id) = Self::call_of(a[1]);
                let (bytes, ..) = self.carrier(a[2]);
                if c != (a[0] - SERVED_BASE) as usize {
                    return Some(Ok(ok(0, 0)));
                }
                let VirtualCell::Port {
                    vocab,
                    calls,
                    room,
                    used,
                    ..
                } = &mut self.host.world.cells[c]
                else {
                    unreachable!()
                };
                let call = calls.get_mut(&id).unwrap();
                if call.finished || call.answer.is_some() {
                    return Some(Ok(ok(0, 0)));
                }
                let dir = (*vocab == Vocab::Dir)
                    .then(|| DirOp::from_word(call.words[0]))
                    .flatten();
                let offer = match (*vocab, call.words[0]) {
                    _ if matches!(dir, Some(DirOp::Read | DirOp::NextEntries)) => call.words[2],
                    _ if matches!(dir, Some(DirOp::Mkdtemp | DirOp::ReadAt | DirOp::Mkstemp)) => {
                        call.words[3]
                    }
                    (Vocab::Net, 1 | 4 | NET_HELLO | NET_DATAGRAM_RECV) => call.words[2],
                    (Vocab::Exec, exec::RECV) => call.words[2].min(call.words[3]),
                    _ => 0,
                };
                let take = offer
                    .saturating_sub(call.staged.len() as u64)
                    .min(bytes.len() as u64);
                let available = *room - *used;
                if take > available {
                    return Some(Ok(vec![take, 1, 0, available]));
                }
                *used += take;
                call.staged.extend_from_slice(&bytes[..take as usize]);
                ok(take, 0)
            }
            HostOp::PortReply => {
                return Some(self.port_reply(a));
            }
            HostOp::ClockSetWall => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Clock {
                    wall, wall_timers, ..
                } = &mut self.host.world.cells[c]
                else {
                    unreachable!()
                };
                *wall = a[1];
                let now = *wall;
                let mut due: Vec<_> = wall_timers
                    .iter()
                    .copied()
                    .filter(|&(at, _)| at <= now)
                    .collect();
                wall_timers.retain(|&(at, _)| at > now);
                due.sort_by_key(|&(at, _)| at);
                for (_, k) in due {
                    self.ready_provider(k);
                }
                ok(0, 0)
            }
            HostOp::ProcessTrigger => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Process { stopped, waiters } = &mut self.host.world.cells[c]
                else {
                    unreachable!()
                };
                *stopped = true;
                for k in std::mem::take(waiters) {
                    self.ready_provider(k);
                }
                ok(0, 0)
            }
            HostOp::ClockIdle => {
                let c = self.cell(a[0]).unwrap();
                if let Err(cancelled) = self.check_cancel(t) {
                    return Some(Err(cancelled));
                }
                let waited = self.cell_wait(
                    t,
                    c,
                    |cell, k| {
                        if let VirtualCell::Clock { idle, .. } = cell
                            && !idle.iter().any(|&(j, _)| j == k)
                        {
                            idle.push((k, false));
                        }
                    },
                    |cell, k| match cell {
                        VirtualCell::Clock { idle, .. } => {
                            idle.iter().any(|&(j, woken)| j == k && woken)
                        }
                        _ => unreachable!(),
                    },
                );
                if let VirtualCell::Clock { idle, .. } = &mut self.host.world.cells[c] {
                    idle.retain(|&(j, _)| j != t.k);
                }
                if let Err(cancelled) = waited {
                    return Some(Err(cancelled));
                }
                let VirtualCell::Clock { timers, .. } = &self.host.world.cells[c] else {
                    unreachable!()
                };
                match timers.iter().map(|&(at, _)| at).min() {
                    Some(at) => ok(at, 1),
                    None => ok(0, 0),
                }
            }
            HostOp::Stdout | HostOp::Stderr => {
                let c = self.cell(a[0])?;
                if let Err(cancelled) = self.check_cancel(t) {
                    return Some(Err(cancelled));
                }
                let (bytes, ..) = self.carrier(a[1]);
                let mut words = [0; 9];
                words[1] = bytes.len() as u64;
                return Some(self.port_call(t, c, words, vec![bytes], None, true));
            }
            HostOp::Open
            | HostOp::OpenDir
            | HostOp::Mkdtemp
            | HostOp::Mkstemp
            | HostOp::Stat
            | HostOp::Mkdir
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::Rename
            | HostOp::SyncDir
            | HostOp::ZeroExtension
            | HostOp::Entries
            | HostOp::Read
            | HostOp::ReadAt
            | HostOp::Write
            | HostOp::WriteAt
            | HostOp::ZeroAt
            | HostOp::StatFile
            | HostOp::Sync
            | HostOp::TryLock
            | HostOp::TryLockShared
            | HostOp::Unlock
            | HostOp::Close
            | HostOp::NextEntries => {
                let (c, h) = self.port_handle(a[0] & !DIR_READ_ONLY)?;
                return Some(self.dir_call(t, op, c, h, a));
            }
            HostOp::WallMs
            | HostOp::MonoNs
            | HostOp::BootNs
            | HostOp::SleepUntil
            | HostOp::SleepUntilWall
            | HostOp::Random
            | HostOp::Stopped => {
                let c = self.cell(a[0])?;
                if !matches!(op, HostOp::WallMs | HostOp::MonoNs | HostOp::BootNs)
                    && let Err(cancelled) = self.check_cancel(t)
                {
                    return Some(Err(cancelled));
                }
                match op {
                    HostOp::WallMs | HostOp::MonoNs | HostOp::BootNs => {
                        let VirtualCell::Clock {
                            mono, boot, wall, ..
                        } = self.host.world.cells[c]
                        else {
                            unreachable!()
                        };
                        vec![if op == HostOp::MonoNs {
                            mono
                        } else if op == HostOp::BootNs {
                            boot
                        } else {
                            wall
                        }]
                    }
                    HostOp::SleepUntil => {
                        let at = a[1];
                        loop {
                            let VirtualCell::Clock { mono, .. } = self.host.world.cells[c] else {
                                unreachable!()
                            };
                            if at <= mono {
                                break;
                            }
                            self.cell_timer(c, at, t.k);
                            self.host.sched.tasks[t.k].cancellable =
                                t.shield == 0 && t.obligations.len() == 1;
                            self.park(Some(Wait::Network(None)));
                            if std::mem::take(&mut self.host.sched.tasks[t.k].cancelled) {
                                if let VirtualCell::Clock { timers, .. } =
                                    &mut self.host.world.cells[c]
                                {
                                    timers.retain(|&(_, k)| k != t.k);
                                }
                                if let Err(cancelled) = self.check_cancel(t) {
                                    return Some(Err(cancelled));
                                }
                            }
                        }
                        ok(0, 0)
                    }
                    HostOp::SleepUntilWall => {
                        let at = a[1];
                        loop {
                            let VirtualCell::Clock { wall, .. } = self.host.world.cells[c] else {
                                unreachable!()
                            };
                            if at <= wall {
                                break;
                            }
                            if let VirtualCell::Clock { wall_timers, .. } =
                                &mut self.host.world.cells[c]
                                && !wall_timers.iter().any(|&(_, j)| j == t.k)
                            {
                                wall_timers.push((at, t.k));
                            }
                            self.host.sched.tasks[t.k].cancellable =
                                t.shield == 0 && t.obligations.len() == 1;
                            self.park(Some(Wait::Network(None)));
                            if std::mem::take(&mut self.host.sched.tasks[t.k].cancelled) {
                                if let VirtualCell::Clock { wall_timers, .. } =
                                    &mut self.host.world.cells[c]
                                {
                                    wall_timers.retain(|&(_, k)| k != t.k);
                                }
                                if let Err(cancelled) = self.check_cancel(t) {
                                    return Some(Err(cancelled));
                                }
                            }
                        }
                        ok(0, 0)
                    }
                    HostOp::Random => return Some(self.draw_virtual_random(c, a[1])),
                    _ => {
                        let waited = self.cell_wait(
                            t,
                            c,
                            |cell, k| {
                                if let VirtualCell::Process { waiters, .. } = cell
                                    && !waiters.contains(&k)
                                {
                                    waiters.push(k);
                                }
                            },
                            |cell, _| matches!(cell, VirtualCell::Process { stopped: true, .. }),
                        );
                        if let VirtualCell::Process { waiters, .. } = &mut self.host.world.cells[c]
                        {
                            waiters.retain(|&k| k != t.k);
                        }
                        if let Err(cancelled) = waited {
                            return Some(Err(cancelled));
                        }
                        vec![]
                    }
                }
            }
            _ => return None,
        };
        if withdrawn(self) {
            return Some(Err(Stop::Cause("Bounds")));
        }
        Some(Ok(reply))
    }
}

pub(super) fn zero_at_error(mode: u8, at: u64, len: u64) -> Option<(u64, u64)> {
    if mode == MODE_DIRECT
        && (!at.is_multiple_of(DIRECT_ALIGNMENT) || !len.is_multiple_of(DIRECT_ALIGNMENT))
    {
        Some((OTHER, 22))
    } else if at.checked_add(len).is_none_or(|end| end > i64::MAX as u64) {
        Some((OTHER, 27))
    } else {
        None
    }
}
