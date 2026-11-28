use super::*;

pub(super) fn next_socket_key(sequence: &mut u64) -> u64 {
    if *sequence >= PORT_KEYS >> 2 {
        std::panic::resume_unwind(Box::new(RunStop("HostHandles")));
    }
    let key = PORT_KEYS | (*sequence << 2);
    *sequence += 1;
    key
}

impl World {
    pub(super) fn port_word(&mut self, base: u64) -> u64 {
        let sequence = self.next_port_word;
        if sequence >= PORT_GRANT {
            std::panic::resume_unwind(Box::new(RunStop("HostHandles")));
        }
        self.next_port_word += 1;
        base + sequence
    }

    pub(super) fn new_port_grant(&mut self, slot: usize) -> u64 {
        let key = self.port_word(PORT_GRANT);
        self.port_grants.insert(key, slot);
        key
    }

    pub(super) fn retire_port(&mut self, slot: usize) {
        let vocab = self.cells[slot].port().4;
        self.free_ports[vocab as usize].push(slot);
        self.port_grants.retain(|key, owner| {
            if *owner != slot {
                return true;
            }
            self.network.grants.remove(key);
            self.exec_grants.remove(key);
            false
        });
        self.port_handles.retain(|_, (c, _)| *c != slot);
        self.port_keys.retain(|key, (c, _, _)| {
            if *c != slot {
                return true;
            }
            self.datagrams.remove(key);
            false
        });
        self.not_public.remove(&Some(slot));
        self.own_address.remove(&Some(slot));
    }
}

impl Machine<'_> {
    pub(super) fn make_port(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        let payer = t.limits.last().unwrap();
        let charge = hirsel_core_layout::charge::PROVIDE_CELL.saturating_add(a[0]);
        if let Err(available) = self.admit(payer, charge) {
            return vec![0, 0, 0, available];
        }
        let vocab = match op {
            HostOp::ProvideDir => Vocab::Dir,
            HostOp::ProvideNet => Vocab::Net,
            HostOp::ProvideExec => Vocab::Exec,
            _ => Vocab::Stream,
        };
        let w = &mut self.host.world;
        let slot = w.free_ports[vocab as usize].pop().unwrap_or(w.cells.len());
        let (calls, next) = if let Some(cell) = w.cells.get_mut(slot) {
            let (calls, _, _, next, _) = cell.port();
            (std::mem::take(calls), *next)
        } else {
            (Default::default(), 0)
        };
        let port = VirtualCell::Port {
            vocab,
            calls,
            next,
            first_call: next,
            size: if vocab == Vocab::Stream { a[1] } else { 0 },
            sealed: false,
            servers: Vec::new(),
            handles: if vocab == Vocab::Dir {
                BTreeMap::from([(0, PortHandle::new(0))])
            } else {
                BTreeMap::new()
            },
            room: a[0],
            used: 0,
        };
        if slot == w.cells.len() {
            w.cells.push(port);
        } else {
            w.cells[slot] = port;
        }
        w.withdrawn.remove(&slot);
        w.cell_charges.insert(slot, (payer.clone(), charge));
        let root = w.new_port_grant(slot);
        match vocab {
            Vocab::Exec => {
                w.exec_grants
                    .insert(root, (slot, hirsel_core_net::exec::ExecRules::universal()));
            }
            Vocab::Net => {
                use hirsel_core_net::{GrantRule, ResolveRule, TlsRule, UnixDirection, UnixRule};
                let tls = GrantRule::Tls(TlsRule::served());
                let resolve = GrantRule::Resolve(ResolveRule::served());
                let unix = UnixDirection::ALL
                    .into_iter()
                    .map(|dir| GrantRule::Unix(UnixRule::served(dir)));
                let rows = GrantRule::served_addresses()
                    .chain([tls, resolve])
                    .chain(unix)
                    .collect();
                w.network.grants.insert(root, rows);
            }
            _ => {}
        }
        vec![root, 0, 0, root - PORT_GRANT + SERVED_BASE]
    }
}

impl World {
    /// Test only operands that designate a port. Constructor sizes, scalar
    /// options, and words of unrelated capabilities may have any bit pattern.
    pub(super) fn stale_port_operand(&self, op: HostOp, a: &[u64]) -> bool {
        let Some(&word) = a.first() else { return false };
        let stale_key = |key: u64| {
            (PORT_KEYS..2 * PORT_KEYS).contains(&key) && !self.port_keys.contains_key(&(key & !3))
        };
        if (op == HostOp::NetJoin || op == HostOp::TlsConnect) && stale_key(a[1]) {
            return true;
        }
        if DirOp::from_host(op.code()).is_some() {
            let key = word & !DIR_READ_ONLY;
            return ((PORT_GRANT..SERVED_BASE).contains(&key)
                && !self.port_grants.contains_key(&key))
                || ((PORT_HANDLE..CALL_BASE).contains(&key)
                    && !self.port_handles.contains_key(&key));
        }
        match op {
            HostOp::Stdout
            | HostOp::Stderr
            | HostOp::StdoutTerminal
            | HostOp::StderrTerminal
            | HostOp::StdoutSize
            | HostOp::StderrSize
            | HostOp::Connect
            | HostOp::Listen
            | HostOp::Resolve
            | HostOp::NetNarrowBegin
            | HostOp::NetDenials
            | HostOp::ExecNarrow
            | HostOp::ExecSpawn
            | HostOp::UnixConnect
            | HostOp::UnixListen
            | HostOp::DatagramOpen
            | HostOp::DatagramBind
            | HostOp::UdpOpen
            | HostOp::UdpBind
            | HostOp::UdpBindGroup
            | HostOp::TlsConnect => {
                (PORT_GRANT..SERVED_BASE).contains(&word) && !self.port_grants.contains_key(&word)
            }
            HostOp::Accept
            | HostOp::LocalAddr
            | HostOp::PeerAddr
            | HostOp::RecvBounded
            | HostOp::RecvReady
            | HostOp::SendPair
            | HostOp::NetSplit
            | HostOp::NetJoin
            | HostOp::TlsHello
            | HostOp::TlsAccept
            | HostOp::TlsServerName
            | HostOp::SendSecret
            | HostOp::UnixAccept
            | HostOp::DatagramConnect
            | HostOp::DatagramSend
            | HostOp::DatagramSendTo
            | HostOp::DatagramRecv
            | HostOp::ExecWait
            | HostOp::ExecSignal => stale_key(word),
            _ => false,
        }
    }
}

impl Machine<'_> {
    /// `provide.next`: the oldest queued call, or None once quiescence answers.
    pub(super) fn port_next(&mut self, t: &Task, root: u64) -> X<Vec<u64>> {
        self.check_cancel(t)?;
        loop {
            let Some(&c) = self.host.world.port_grants.get(&root) else {
                return Err(Stop::Cause("Bounds"));
            };
            let queued = self.host.world.cells[c].queued();
            let VirtualCell::Port {
                vocab,
                calls,
                servers,
                handles,
                used,
                next,
                ..
            } = &mut self.host.world.cells[c]
            else {
                unreachable!()
            };
            let quiesced = servers.iter().any(|&(k, q)| k == t.k && q);
            if queued.is_some() || quiesced {
                servers.retain(|&(k, _)| k != t.k);
                let id = match queued {
                    None => return Ok(vec![0, 0, 0, 0]),
                    Some(Queued::Call(id)) => {
                        let call = calls.get_mut(&id).unwrap();
                        call.taken = true;
                        call.held = true;
                        id
                    }
                    Some(Queued::Released(h, write)) => {
                        // The notice's record leaves the room with the last
                        // part's notice, and becomes a Call that needs no reply.
                        let handle = handles.get_mut(&h).unwrap();
                        let part = match (handle.kind, handle.write, write) {
                            (PORT_PIPE_READ, _, _) | (_, Some(_), false) => 1,
                            (PORT_PIPE_WRITE, _, _) | (_, Some(_), true) => 2,
                            _ => 0,
                        };
                        if write {
                            handle.write = Some(PortState::Gone);
                        } else {
                            handle.state = PortState::Gone;
                        }
                        if handle.done() {
                            handles.remove(&h);
                            *used -= HANDLE_ROOM;
                        }
                        let id = *next;
                        *next += 1;
                        let mut words = [0; 9];
                        match *vocab {
                            Vocab::Net => words[..3].copy_from_slice(&[NET_RELEASED, h, part]),
                            Vocab::Exec => words[..3].copy_from_slice(&[exec::RELEASED, h, part]),
                            _ => words[..2].copy_from_slice(&[DirOp::Released.word(), h]),
                        }
                        calls.insert(
                            id,
                            PortCall {
                                caller: t.k,
                                words,
                                fields: Vec::new(),
                                out: None,
                                staged: Vec::new(),
                                taken: true,
                                held: true,
                                answer: None,
                                finished: true,
                                secret: None,
                            },
                        );
                        id
                    }
                };
                if id > u64::from(u32::MAX) || c as u64 >= CALL_BASE >> 32 {
                    std::panic::resume_unwind(Box::new(RunStop("HostHandles")));
                }
                return Ok(vec![CALL_BASE + ((c as u64) << 32) + id, 0, 0, 1]);
            }
            if !servers.iter().any(|&(k, _)| k == t.k) {
                servers.push((t.k, false));
            }
            self.host.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
            self.park(Some(Wait::Network(None)));
            if std::mem::take(&mut self.host.sched.tasks[t.k].cancelled) {
                if let VirtualCell::Port { servers, .. } = &mut self.host.world.cells[c] {
                    servers.retain(|&(k, _)| k != t.k);
                }
                self.check_cancel(t)?;
            }
        }
    }
}
