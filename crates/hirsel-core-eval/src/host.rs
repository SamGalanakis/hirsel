use super::*;

impl Machine<'_> {
    /// NET-18 part 2: a served Net connect only `Public` rules cover, which
    /// its server is told.
    pub(super) fn public_only(&self, a: &[u64]) -> bool {
        self.host
            .world
            .network
            .grants
            .get(&a[0])
            .is_some_and(|rules| {
                let rules = rules.iter().filter_map(|rule| match rule {
                    hirsel_core_net::GrantRule::Address(rule) => Some(*rule),
                    _ => None,
                });
                hirsel_core_net::public_only(rules, a[1] as u8, a[2], a[3], a[4] as u16)
            })
    }

    /// A public-only connect its server answered `Denied` counts as `own_address`.
    pub(super) fn own_address(&mut self, public: bool, port: usize, row: Vec<u64>) -> Vec<u64> {
        if public && row[1] == 1 {
            *self.host.world.own_address.entry(Some(port)).or_default() += 1;
        }
        row
    }

    /// A provider row against [`World`]: an outcome `value, error, code, written`.
    pub(super) fn host(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        if op == HostOp::SecretSeal {
            return Ok(self.secret_policy(t, op, a));
        }
        self.deny_describe_capability();
        if op == HostOp::StdinSecret {
            return self.stdin_secret(t, a);
        }
        let mode = if op == HostOp::Open { a[2] as u8 } else { 0 };
        if needs_writable_dir(op.code(), mode) && a[0] & DIR_READ_ONLY != 0 {
            self.check_cancel(t)?;
            return Ok(vec![0, DENIED, 0, 0]);
        }
        if op == HostOp::Entries
            && a[1] != 0
            && !hirsel_core_net::valid_relative_path(self.carrier_bytes(a[1]))
        {
            self.check_cancel(t)?;
            return Ok(vec![0, DENIED, 0, 0]);
        }
        if let Some(reply) = self.provide(t, op, a) {
            return reply;
        }
        if op.is_media() {
            return self.media_call(t, op, a);
        }
        match op {
            HostOp::TlsConnect => return self.tls_connect(t, a),
            HostOp::TlsHello => return self.tls_hello(t, a),
            HostOp::TlsAccept => return self.tls_accept(t, a),
            HostOp::TlsServerName => {
                let name = self.host.world.tls_hellos.get(&a[0]).cloned();
                return Ok(vec![self.host_view(&name.unwrap_or_default()), 0, 0, 0]);
            }
            HostOp::SendSecret => return self.send_secret(t, a),
            HostOp::ScramSha256 => return self.scram_send(t, a),
            HostOp::ScramFinalLen => return Ok(self.scram_final_len(a)),
            HostOp::ScramVerify => return Ok(self.scram_verify(a)),
            HostOp::SecretNarrow => return Ok(self.secret_policy(t, op, a)),
            HostOp::SecretDeriveHmac
            | HostOp::SecretDeriveDigest
            | HostOp::SecretDeriveHkdf
            | HostOp::SecretDeriveArgon2id => {
                return self.secret_derive(t, op, a);
            }
            op if op.is_secret_mac() => return self.secret_mac(t, op, a),
            HostOp::SecretP256Key => return self.secret_p256_key(t, a),
            HostOp::SecretKeyShare => return self.key_share(t, a),
            HostOp::SecretWrap | HostOp::SecretUnwrap | HostOp::SecretSign => {
                return self.secret_key(t, op, a);
            }
            HostOp::SecretAeadSeal | HostOp::SecretAeadOpen => return self.secret_aead(t, op, a),
            HostOp::EnvGet | HostOp::EnvLen | HostOp::EnvName | HostOp::EnvNarrow => {
                return Ok(self.env(t, op, a));
            }
            HostOp::SendPair
                if self
                    .host
                    .world
                    .tls_writes
                    .get(&(a[0] & !3))
                    .is_some_and(|record| record.1) =>
            {
                return Ok(vec![0, 5, 0, 0]);
            }
            _ => {}
        }
        let ok = |value: u64, written: u64| vec![value, 0, 0, written];
        let error = |e: u64| vec![0, e, 0, 0];
        let resolve = |(tag, code)| vec![0, tag, code, 0];
        let stat = |kind: u64, size: u64, time: u64| vec![size, 0, 0, time, kind];
        let stat_error = |(tag, code)| vec![0, tag, code, 0, 0];
        let file = |h: u64| (h - FILE_BASE) as usize;
        // Every operation but a clock read is a cancellation point at entry
        // (native provider `drive`, `cancellation_point`).
        if !matches!(
            op,
            HostOp::WallMs
                | HostOp::MonoNs
                | HostOp::BootNs
                | HostOp::ExecNarrow
                | HostOp::NetNarrow
                | HostOp::NetClass
                | HostOp::NetDenials
                | HostOp::NetNarrowBegin
                | HostOp::NetNarrowEnd
                | HostOp::NetNarrowTls
                | HostOp::NetNarrowResolve
                | HostOp::NetNarrowUnix
                | HostOp::NetSplit
                | HostOp::NetJoin
        ) {
            self.check_cancel(t)?;
        }
        Ok(match op {
            HostOp::Connect
            | HostOp::Listen
            | HostOp::Accept
            | HostOp::LocalAddr
            | HostOp::PeerAddr
            | HostOp::RecvBounded
            | HostOp::RecvReady
            | HostOp::SendPair => {
                return self.network_host(t, op, a);
            }
            HostOp::TlsConnect
            | HostOp::TlsHello
            | HostOp::TlsAccept
            | HostOp::TlsServerName
            | HostOp::SendSecret
            | HostOp::ScramSha256
            | HostOp::ScramFinalLen
            | HostOp::ScramVerify
            | HostOp::SecretSeal
            | HostOp::SecretNarrow
            | HostOp::SecretDeriveHmac
            | HostOp::SecretDeriveDigest
            | HostOp::SecretDeriveHkdf
            | HostOp::SecretDeriveArgon2id
            | HostOp::SecretHmac
            | HostOp::SecretHmacSha1
            | HostOp::SecretVerifyHmacSha1
            | HostOp::SecretVerifyHmac
            | HostOp::SecretHmacSha384
            | HostOp::SecretHmacSha512
            | HostOp::SecretVerifyHmacSha384
            | HostOp::SecretVerifyHmacSha512
            | HostOp::SecretJwsHmacSign
            | HostOp::SecretJwsHmacVerify
            | HostOp::SecretWrap
            | HostOp::SecretUnwrap
            | HostOp::SecretSign
            | HostOp::SecretP256Key
            | HostOp::SecretKeyShare
            | HostOp::ScheduleQuicInitial
            | HostOp::ScheduleImportTraffic
            | HostOp::ScheduleTls13
            | HostOp::ScheduleApplication
            | HostOp::ScheduleKeyUpdate
            | HostOp::ScheduleRetire
            | HostOp::ScheduleFinished
            | HostOp::ScheduleVerifyFinished
            | HostOp::ScheduleDtls12
            | HostOp::ScheduleImportSrtp
            | HostOp::ScheduleSrtp
            | HostOp::ScheduleSpaceInfo
            | HostOp::ScheduleSealerPair
            | HostOp::ScheduleImportSframe
            | HostOp::ScheduleSframeSealer
            | HostOp::ScheduleSframeOpener
            | HostOp::ScheduleSframeRatchet
            | HostOp::ScheduleSealer
            | HostOp::ScheduleOpener
            | HostOp::ScheduleNextOpener
            | HostOp::ScheduleHeaderMask
            | HostOp::PacketSeal
            | HostOp::PacketSealInto
            | HostOp::PacketOpen
            | HostOp::PacketOpenInto
            | HostOp::PacketMask
            | HostOp::SecretAeadSeal
            | HostOp::SecretAeadOpen
            | HostOp::EnvGet
            | HostOp::EnvLen
            | HostOp::EnvName
            | HostOp::EnvNarrow
            | HostOp::ProvideClock
            | HostOp::ProvideRandom
            | HostOp::ProvideRandomTape
            | HostOp::ProvideProcess
            | HostOp::ClockAdvance
            | HostOp::ClockAdvanceBoot
            | HostOp::ClockSetWall
            | HostOp::ClockIdle
            | HostOp::ProcessTrigger
            | HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::PortNext
            | HostOp::CallWord
            | HostOp::CallSecret
            | HostOp::CallPayload
            | HostOp::CallPayloadPrefix
            | HostOp::PortReply
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec
            | HostOp::PortFill
            | HostOp::StdinTerminal
            | HostOp::StdoutTerminal
            | HostOp::StderrTerminal
            | HostOp::StdoutSize
            | HostOp::StderrSize
            | HostOp::StdinSecret => {
                unreachable!()
            }
            HostOp::CheckLibrary => {
                let reply = self
                    .host
                    .world
                    .compiler
                    .as_ref()
                    .filter(|_| a[0] == 0)
                    .map(|check| check(self.carrier_bytes(a[1])));
                let (tag, bytes) =
                    reply.unwrap_or_else(|| (9, b"compiler service is unavailable".to_vec()));
                let size = bytes.len() as u64;
                if size != 0 {
                    let limit = t.limits.last().unwrap().clone();
                    let charge = hirsel_core_layout::charge::seq(size, 1).unwrap();
                    if let Err(available) = self.admit(&limit, charge) {
                        return Ok(vec![charge, 1, 0, available]);
                    }
                    let ptr = self.block(size, Some(limit), charge, None);
                    self.put(ptr, &bytes);
                    self.store(a[2], 8, ptr);
                    self.store(a[2] + 8, 8, size);
                    self.store(a[2] + 16, 8, size);
                }
                vec![tag, 0, 0, 0]
            }
            HostOp::CredFetch => unreachable!("a model world has no Cred"),
            HostOp::ExecSpawn | HostOp::ExecWait | HostOp::ExecSignal => {
                return self.exec_call(t, op, a);
            }
            HostOp::ExecNarrow => self.exec_narrow(t, a),
            HostOp::Resolve => return self.network_resolve(t, a),
            HostOp::UnixConnect
            | HostOp::UnixListen
            | HostOp::UnixAccept
            | HostOp::DatagramOpen
            | HostOp::DatagramBind
            | HostOp::DatagramConnect
            | HostOp::DatagramSend
            | HostOp::DatagramSendTo
            | HostOp::DatagramRecv => {
                return self.unix_host(t, op, a);
            }
            HostOp::NetNarrowUnix => {
                use hirsel_core_net::{GrantRule, UnixDirection, UnixRule};
                let rule = UnixDirection::from_tag(a[1] as u8).and_then(|dir| {
                    let root = std::str::from_utf8(self.carrier_bytes(a[2])).ok()?;
                    let path = std::str::from_utf8(self.carrier_bytes(a[3])).ok()?;
                    UnixRule::new(dir, root, path)
                });
                self.narrow_add(a[0], rule.map(GrantRule::Unix))
            }
            HostOp::NetNarrowResolve => {
                use hirsel_core_net::{GrantRule, ResolveRule};
                let rule = std::str::from_utf8(self.carrier_bytes(a[1]))
                    .ok()
                    .and_then(ResolveRule::new);
                self.narrow_add(a[0], rule.map(GrantRule::Resolve))
            }
            HostOp::NetNarrowTls => {
                use hirsel_core_net::{GrantRule, TlsRule, TlsTrust};
                let trust = match a[4] {
                    0 => Some(TlsTrust::System),
                    1 => self
                        .carrier_bytes(a[5])
                        .try_into()
                        .ok()
                        .map(TlsTrust::Roots),
                    _ => None,
                };
                let rule = trust.and_then(|trust| {
                    let patterns = std::str::from_utf8(self.carrier_bytes(a[1])).ok()?;
                    TlsRule::new(patterns, a[2] as u16, a[3] as u16, trust)
                });
                self.narrow_add(a[0], rule.map(GrantRule::Tls))
            }
            HostOp::NetSplit if self.port_socket_part(a[0], true) => {
                vec![a[0] | 1, 0, 0, a[0] | 2]
            }
            HostOp::NetSplit => {
                let (read, write) = self.host.world.network.sockets.split(a[0]);
                vec![read, 0, 0, write]
            }
            HostOp::NetJoin
                if (a[0] & !3) == (a[1] & !3)
                    && !self.host.world.tls_writes.contains_key(&(a[0] & !3))
                    && self.port_join(a[0]) =>
            {
                ok(a[0] & !3, 0)
            }
            HostOp::NetJoin => vec![a[0], 1, 0, a[1]],
            HostOp::UdpOpen | HostOp::UdpRecv | HostOp::UdpSend | HostOp::UdpFillPackets => {
                error(1)
            }
            HostOp::UdpBind | HostOp::UdpBindGroup | HostOp::UdpLocalAddr => {
                vec![0, 1, 0, 0, 0, 0, 0, 0]
            }
            HostOp::NetClass => unreachable!("net.class runs on the pure core"),
            HostOp::NetDenials => {
                let provider = self.host.world.net_ports.get(&a[0]).copied();
                ok(
                    self.host
                        .world
                        .not_public
                        .get(&provider)
                        .copied()
                        .unwrap_or(0),
                    self.host
                        .world
                        .own_address
                        .get(&provider)
                        .copied()
                        .unwrap_or(0),
                )
            }
            HostOp::NetNarrow => {
                let rule = hirsel_core_net::Rule::from_core(hirsel_core_net::RuleRow {
                    dir: a[1] as u8,
                    family: a[2] as u8,
                    hi: a[3],
                    lo: a[4],
                    bits: a[5] as u8,
                    reach: a[6] as u8,
                    first: a[7] as u16,
                    last: a[8] as u16,
                });
                self.narrow_add(a[0], rule.map(hirsel_core_net::GrantRule::Address))
            }
            HostOp::NetNarrowBegin => {
                let narrowing = &mut self.host.world.narrowing;
                let key = narrowing.last_key_value().map_or(1, |(&key, _)| key + 1);
                narrowing.insert(key, (a[0], Vec::new(), false));
                ok(key, 0)
            }
            HostOp::NetNarrowEnd => {
                let (parent, rules, invalid) = self
                    .host
                    .world
                    .narrowing
                    .remove(&a[0])
                    .expect("a list narrow ends the builder it began");
                if invalid {
                    error(1)
                } else {
                    self.narrow_net(t, parent, &rules)
                }
            }
            HostOp::StdinRead => {
                let (data, len, cap) = (
                    self.load(a[1], 8),
                    self.load(a[1] + 8, 8),
                    self.load(a[1] + 16, 8),
                );
                let n = (cap.saturating_sub(len) as usize)
                    .min(self.host.world.stdin.len())
                    .min(64 * 1024);
                if n != 0 {
                    let bytes: Vec<_> = self.host.world.stdin.drain(..n).collect();
                    self.put(data + len, &bytes);
                    self.store(a[1] + 8, 8, len + n as u64);
                }
                ok(n as u64, 0)
            }
            HostOp::Stopped => {
                // The model has no OS signal source. Cancellation still wakes this wait.
                self.host.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
                self.park(Some(Wait::Stop));
                self.check_cancel(t)?;
                vec![]
            }
            HostOp::SleepUntil | HostOp::SleepUntilWall => {
                // Ready if the deadline has passed, else park in W until its
                // timer (os.rs `sleep_wait`); a cancellation that takes effect
                // there fails the sleep Cancelled. The model's wall and
                // monotonic readings are one counter, so a wall wait is this
                // same wait.
                let deadline = a[1];
                if deadline > self.host.world.clock {
                    self.host.sched.tasks[t.k].cancellable =
                        t.shield == 0 && t.obligations.len() == 1;
                    self.park(Some(Wait::Sleep(deadline)));
                    if std::mem::take(&mut self.host.sched.tasks[t.k].cancelled) {
                        self.check_cancel(t)?;
                    }
                }
                ok(0, 0)
            }
            HostOp::WallMs | HostOp::MonoNs | HostOp::BootNs => {
                // The host clock is monotonic (native reads CLOCK_MONOTONIC,
                // hirsel-core-rt os::monotonic_ns): a read after a sleep to the
                // maximum deadline stays there instead of wrapping (CONC-7).
                self.host.world.clock = self.host.world.clock.saturating_add(1);
                vec![self.host.world.clock]
            }
            HostOp::Stdout | HostOp::Stderr => {
                let (bytes, ..) = self.carrier(a[1]);
                let n = bytes.len() as u64;
                match op {
                    HostOp::Stdout => self.host.world.stdout.extend(bytes),
                    _ => self.host.world.stderr.extend(bytes),
                }
                ok(0, n)
            }
            HostOp::Random => {
                let (_, data, len, cap) = self.carrier(a[1]);
                let spare = cap.saturating_sub(len) as usize;
                let bytes = (0..spare)
                    .map(|i| self.host.world.random.wrapping_add(i as u8))
                    .collect::<Vec<_>>();
                self.host.world.random = self.host.world.random.wrapping_add(spare as u8);
                self.put(data + len, &bytes);
                self.store(a[1] + 8, 8, len + spare as u64);
                ok(0, spare as u64)
            }
            HostOp::Open => {
                let key = match self.host.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                let w = &mut self.host.world;
                let mode = a[2] as u8;
                let file = match (w.nodes.get(&key), mode) {
                    (Some(Node::Dir(_)), _) => return Ok(error(DENIED)),
                    (None, 0) => return Ok(error(NOT_FOUND)),
                    (Some(_), 3) => return Ok(error(EXISTS)),
                    (Some(Node::File(file)), _) => Arc::clone(file),
                    (None, _) => {
                        let file = Arc::new(Mutex::new(ModelFile {
                            modified_ns: w.clock.min(i64::MAX as u64),
                            ..ModelFile::default()
                        }));
                        w.nodes.insert(key, Node::File(Arc::clone(&file)));
                        file
                    }
                };
                if matches!(mode, 1 | MODE_DURABLE | MODE_DIRECT) {
                    *file.lock().unwrap() = ModelFile {
                        modified_ns: w.clock.min(i64::MAX as u64),
                        ..ModelFile::default()
                    };
                }
                w.files.push(Some(OpenFile {
                    file,
                    at: 0,
                    lock: None,
                    mode,
                    read_only_dir: a[0] & DIR_READ_ONLY != 0,
                }));
                ok(FILE_BASE + w.files.len() as u64 - 1, 0)
            }
            HostOp::OpenDir => match self.host.world.find_dir(a[0], self.carrier_bytes(a[1])) {
                Ok(dir) => ok(dir | (a[0] & DIR_READ_ONLY), 0),
                Err(e) => resolve(e),
            },
            HostOp::Entries => {
                let dir = if a[1] == 0 {
                    a[0]
                } else {
                    match self.host.world.find_dir(a[0], self.carrier_bytes(a[1])) {
                        Ok(dir) => dir,
                        Err(e) => return Ok(resolve(e)),
                    }
                };
                let records = self
                    .host
                    .world
                    .children(dir)
                    .map(|((_, name), node)| {
                        let kind = u8::from(matches!(node, Node::Dir(_)));
                        let mut record = vec![kind];
                        record.extend_from_slice(&(name.len() as u16).to_le_bytes());
                        record.extend_from_slice(name);
                        record
                    })
                    .collect();
                let h = LISTING_HANDLE | self.host.world.listings.len() as u64;
                self.host.world.listings.push(Some(records));
                ok(h, 0)
            }
            HostOp::NextEntries => {
                let (_, data, len, cap) = self.carrier(a[1]);
                let spare = cap.saturating_sub(len);
                if spare < 258 {
                    return Err(Stop::Cause("Bounds"));
                }
                let Some(records) =
                    self.host.world.listings[(a[0] & !LISTING_HANDLE) as usize].as_mut()
                else {
                    return Ok(error(CLOSED));
                };
                let mut bytes = Vec::new();
                let mut count = 0;
                while records
                    .front()
                    .is_some_and(|r| bytes.len() as u64 + r.len() as u64 <= spare)
                {
                    bytes.extend(records.pop_front().unwrap());
                    count += 1;
                }
                if count != 0 {
                    self.put(data + len, &bytes);
                    self.store(a[1] + 8, 8, len + bytes.len() as u64);
                }
                ok(count, 0)
            }
            HostOp::Stat => {
                let key = match self.host.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(stat_error(e)),
                };
                match self.host.world.nodes.get(&key) {
                    Some(Node::Dir(_)) => stat(1, 0, 0),
                    Some(Node::File(f)) => {
                        let f = f.lock().unwrap();
                        stat(0, f.len, f.modified_ns)
                    }
                    None => stat_error((NOT_FOUND, 0)),
                }
            }
            HostOp::StatFile => {
                if let Some(f) = &self.host.world.files[file(a[0])] {
                    let f = f.file.lock().unwrap();
                    stat(0, f.len, f.modified_ns)
                } else {
                    stat_error((CLOSED, 0))
                }
            }
            HostOp::Read | HostOp::ReadAt => {
                let positioned = op == HostOp::ReadAt;
                let buffer = a[if positioned { 2 } else { 1 }];
                let (_, data, len, cap) = self.carrier(buffer);
                let i = file(a[0]);
                let Some(f) = self.host.world.files[i].as_ref() else {
                    return Ok(error(CLOSED));
                };
                if positioned && !matches!(f.mode, 0 | 4) {
                    return Ok(error(DENIED));
                }
                let at = if positioned { a[1] } else { f.at };
                let (n, bytes) = {
                    let c = f.file.lock().unwrap();
                    let n = cap.saturating_sub(len).min(c.len.saturating_sub(at)) as usize;
                    (n, c.read(at, n))
                };
                if n > 0 {
                    self.put(data + len, &bytes);
                    self.store(buffer + 8, 8, len + n as u64);
                }
                if !positioned {
                    self.host.world.files[i].as_mut().unwrap().at += n as u64;
                }
                ok(n as u64, 0)
            }
            HostOp::Write | HostOp::WriteAt => {
                let positioned = op == HostOp::WriteAt;
                let (bytes, data, ..) = self.carrier(a[if positioned { 2 } else { 1 }]);
                let i = file(a[0]);
                if positioned
                    && self.host.world.files[i]
                        .as_ref()
                        .is_some_and(|f| f.mode == MODE_DIRECT)
                {
                    let aligned = |value: u64| value.is_multiple_of(DIRECT_ALIGNMENT);
                    if !aligned(a[1])
                        || !aligned(bytes.len() as u64)
                        || !self.direct_buffer_aligned(data)
                    {
                        return Ok(vec![0, 5, 22, 0]); // Other(EINVAL)
                    }
                }
                if self.host.world.files[i]
                    .as_ref()
                    .is_some_and(|f| f.read_only_dir)
                {
                    return Ok(error(DENIED));
                }
                if positioned && a[1].checked_add(bytes.len() as u64).is_none() {
                    return Err(Stop::Cause("Bounds"));
                }
                let Some(f) = self.host.world.files[i].as_mut() else {
                    return Ok(error(CLOSED));
                };
                if positioned && !matches!(f.mode, 1 | 3 | 4 | MODE_DURABLE | MODE_DIRECT) {
                    return Ok(error(DENIED));
                }
                if bytes.is_empty() {
                    return Ok(ok(0, 0));
                }
                let mut c = f.file.lock().unwrap();
                let at = if positioned {
                    a[1]
                } else if f.mode == 2 {
                    c.len
                } else {
                    f.at
                };
                let end = at + bytes.len() as u64;
                if end > i64::MAX as u64 {
                    return Ok(vec![0, 5, 27, 0]); // Other(EFBIG)
                }
                c.write(at, &bytes);
                c.modified_ns = self.host.world.clock.min(i64::MAX as u64);
                if !positioned {
                    f.at = end;
                }
                ok(0, bytes.len() as u64)
            }
            HostOp::ZeroAt => {
                let index = file(a[0]);
                let Some(handle) = self.host.world.files[index].as_ref() else {
                    return Ok(error(CLOSED));
                };
                if handle.read_only_dir
                    || !matches!(handle.mode, 1 | 3 | 4 | MODE_DURABLE | MODE_DIRECT)
                {
                    return Ok(error(DENIED));
                }
                let (at, len) = (a[1], a[2]);
                if let Some(problem) = zero_at_error(handle.mode, at, len) {
                    return Ok(resolve(problem));
                }
                if len != 0 {
                    let mut target = handle.file.lock().unwrap();
                    target.zero_at(at, len);
                    target.modified_ns = self.host.world.clock.min(i64::MAX as u64);
                }
                ok(0, 0)
            }
            HostOp::Close => {
                let i = file(a[0]);
                self.host.world.files[i] = None;
                ok(0, 0)
            }
            HostOp::TryLock | HostOp::TryLockShared | HostOp::Unlock => {
                let index = file(a[0]);
                let requested = match op {
                    HostOp::TryLock => Some(FileLock::Exclusive),
                    HostOp::TryLockShared => Some(FileLock::Shared),
                    _ => None,
                };
                let open = self.host.world.files[index].as_ref().expect("an open file");
                let grant = requested.filter(|&wanted| {
                    !self
                        .host
                        .world
                        .files
                        .iter()
                        .enumerate()
                        .any(|(other, handle)| {
                            other != index
                                && handle.as_ref().is_some_and(|handle| {
                                    Arc::ptr_eq(&handle.file, &open.file)
                                        && handle.lock.is_some_and(|held| {
                                            wanted == FileLock::Exclusive
                                                || held == FileLock::Exclusive
                                        })
                                })
                        })
                });
                self.host.world.files[index].as_mut().unwrap().lock = grant;
                ok(u64::from(requested.is_none() || grant.is_some()), 0)
            }
            HostOp::Mkdir => {
                let key = match self.host.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                if self.host.world.nodes.contains_key(&key) {
                    return Ok(error(EXISTS));
                }
                self.host.world.new_dir(key);
                ok(0, 0)
            }
            HostOp::Rename => {
                let from = match self.host.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                let to = match self.host.world.dir_key(a[0], self.carrier_bytes(a[2])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                // core.md: a regular file `from` takes the name `to` in the one `Dir`,
                // replacing a regular file there; anything else at either name is
                // `Denied` and renames nothing.
                match self.host.world.nodes.get(&from) {
                    None => return Ok(error(NOT_FOUND)),
                    Some(Node::Dir(_)) => return Ok(error(DENIED)),
                    Some(Node::File(_)) => {}
                }
                if let Some(Node::Dir(_)) = self.host.world.nodes.get(&to) {
                    return Ok(error(DENIED));
                }
                // The same name: removed and put back, unchanged.
                let node = self.host.world.nodes.remove(&from).unwrap();
                self.host.world.nodes.insert(to, node);
                ok(0, 0)
            }
            HostOp::Remove | HostOp::RemoveTree => {
                let key = match self.host.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                if let Some(Node::Dir(dir)) = self.host.world.nodes.get(&key) {
                    let dir = *dir;
                    if op == HostOp::RemoveTree {
                        self.host.world.remove_children(dir);
                    } else if self.host.world.children(dir).next().is_some() {
                        return Ok(resolve((OTHER, 39))); // ENOTEMPTY
                    }
                }
                match self.host.world.nodes.remove(&key) {
                    Some(_) => ok(0, 0),
                    None => error(NOT_FOUND),
                }
            }
            HostOp::Mkdtemp | HostOp::Mkstemp => {
                let (prefix, data, len, cap) = self.carrier(a[1]);
                if cap.saturating_sub(len) < 32 {
                    return Err(Stop::Cause("Bounds"));
                }
                if prefix.iter().any(|&b| b == b'/' || b == 0) {
                    return Ok(error(DENIED));
                }
                for _ in 0..128 {
                    let k = self.host.world.temporaries;
                    self.host.world.temporaries += 1;
                    let suffix = format!("t{k}").into_bytes();
                    let full = [prefix.as_slice(), &suffix].concat();
                    if full.len() > 255 {
                        return Ok(resolve((OTHER, 36))); // ENAMETOOLONG
                    }
                    let key = (a[0], full);
                    if !self.host.world.nodes.contains_key(&key) {
                        self.put(data + len, &suffix);
                        self.store(a[1] + 8, 8, len + suffix.len() as u64);
                        if op == HostOp::Mkdtemp {
                            return Ok(ok(self.host.world.new_dir(key), 0));
                        }
                        let file = Arc::new(Mutex::new(ModelFile {
                            modified_ns: self.host.world.clock.min(i64::MAX as u64),
                            ..ModelFile::default()
                        }));
                        self.host
                            .world
                            .nodes
                            .insert(key, Node::File(Arc::clone(&file)));
                        self.host.world.files.push(Some(OpenFile {
                            file,
                            at: 0,
                            lock: None,
                            mode: 3,
                            read_only_dir: false,
                        }));
                        return Ok(ok(FILE_BASE + self.host.world.files.len() as u64 - 1, 0));
                    }
                }
                resolve((OTHER, 11)) // EAGAIN
            }
            HostOp::Sync | HostOp::SyncDir | HostOp::ZeroExtension => ok(0, 0),
        })
    }
}
