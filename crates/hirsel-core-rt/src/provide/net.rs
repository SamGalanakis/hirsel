use super::*;

/// Every served Net starts with the same immutable rules. Datagrams and UDP
/// rules caches may still name this view after the port that issued it ends.
pub(crate) fn served_rules() -> u64 {
    struct Root(crate::net::RulesStorage);
    // SAFETY: RulesStorage owns both boxed allocations. Its view points into
    // the immutable rows and the process-wide root never moves or drops them.
    unsafe impl Sync for Root {}
    unsafe impl Send for Root {}
    static ROOT: OnceLock<Root> = OnceLock::new();
    ROOT.get_or_init(|| {
        use hirsel_core_net::{GrantRule, ResolveRule, TlsRule, UnixDirection, UnixRule};
        let tls = GrantRule::Tls(TlsRule::served());
        let resolve = GrantRule::Resolve(ResolveRule::served());
        let unix = UnixDirection::ALL
            .into_iter()
            .map(|dir| GrantRule::Unix(UnixRule::served(dir)));
        Root(crate::net::RulesStorage::new(
            GrantRule::served_addresses()
                .chain([tls, resolve])
                .chain(unix),
        ))
    })
    .0
    .handle()
}

impl Port {
    fn admits_grant(&self, handle: u64) -> bool {
        !self.withdrawn && (handle == self.root || self.grants.contains(&handle))
    }

    pub(super) fn stale(&self, request: &Request) -> bool {
        let handle = request.handle;
        if handle & PORT_ROOT != 0 || self.vocab == Vocab::Stream {
            return handle != self.root;
        }
        let root = matches!(
            request.operation,
            CONNECT
                | LISTEN
                | RESOLVE
                | EXEC_SPAWN
                | DATAGRAM_OPEN
                | UDP_OPEN
                | UDP_BIND
                | UDP_BIND_GROUP
        ) || UnixOperation::describe(request.operation)
            .is_some_and(|op| matches!(op.fields.first(), Some(UnixField::Root(_))));
        if root {
            return !self.admits_grant(handle);
        }
        (PORT_KEYS..2 * PORT_KEYS).contains(&handle) && !self.keys.contains_key(&(handle & !3))
    }

    pub(super) fn net_words(&self, request: &Request) -> Option<[u64; 9]> {
        if udp_operation(request.operation) {
            return self.udp_words(request);
        }
        if let Some(op) = UnixOperation::describe(request.operation) {
            // SAFETY: the Unix adapter pins this extension through delivery.
            let n = unsafe { &*request.path.cast::<UnixRequest>() };
            let mut w = [0; 9];
            w[0] = op.served;
            let b = request.buffer;
            for (word, field) in w[1..].iter_mut().zip(op.fields) {
                *word = match field {
                    UnixField::Listener => self.socket(request.handle, Kind::UnixListener)?,
                    UnixField::Datagram => self.socket(request.handle, Kind::Datagram)?,
                    UnixField::Root(_) => n.root.len() as u64,
                    UnixField::Path => n.path.len() as u64,
                    UnixField::Backlog => u64::from(n.backlog),
                    UnixField::Bytes => b.len as u64,
                    UnixField::Room => b.capacity.saturating_sub(b.len) as u64,
                };
            }
            return Some(w);
        }
        // SAFETY: every Net adapter pins its NetworkRequest in `path`.
        let n = unsafe { &*request.path.cast::<crate::net::NetworkRequest>() };
        let a = n.address;
        let b = request.buffer;
        let spare = (b.capacity.saturating_sub(b.len)) as u64;
        let address = [u64::from(a.family), a.hi, a.lo, u64::from(a.port)];
        let mut w = [0; 9];
        match request.operation {
            CONNECT => {
                w[0] = NET_CONNECT;
                w[1..5].copy_from_slice(&address);
                // NET-18: `public`, so the server refuses its own addresses.
                w[5] = u64::from(n.public);
            }
            RESOLVE => {
                // The name as the caller wrote it, which its rules covered,
                // and room for that many address records (NOTES 34).
                // SAFETY: RESOLVE pins its ResolveRequest in `path`.
                let name = unsafe {
                    (*request.path.cast::<crate::resolve::ResolveRequest>())
                        .raw_name
                        .len
                };
                let room = spare.saturating_mul(hirsel_core_net::IP_RECORD_LEN as u64);
                w[..3].copy_from_slice(&[NET_RESOLVE, name as u64, room]);
            }
            LISTEN => {
                w[0] = NET_LISTEN;
                w[1..5].copy_from_slice(&address);
                w[5] = u64::from(n.backlog);
            }
            ACCEPT => {
                let id = self.socket(request.handle, Kind::Listener)?;
                w[..2].copy_from_slice(&[NET_ACCEPT, id]);
            }
            RECV_BOUNDED => {
                let id = self.socket(request.handle, Kind::Socket)?;
                w[..4].copy_from_slice(&[NET_RECV, id, spare, n.max_receive]);
            }
            RECV_READY => {
                let id = self.socket(request.handle, Kind::Socket)?;
                w[..2].copy_from_slice(&[NET_READY, id]);
            }
            LOCAL_ADDR => {
                let id = self
                    .socket(request.handle, Kind::Socket)
                    .or_else(|| self.socket(request.handle, Kind::Listener))?;
                w[..2].copy_from_slice(&[NET_LOCAL_ADDR, id]);
            }
            PEER_ADDR => {
                let id = self.socket(request.handle, Kind::Socket)?;
                w[..2].copy_from_slice(&[NET_PEER_ADDR, id]);
            }
            SEND_PAIR => {
                let id = self.socket(request.handle, Kind::Socket)?;
                // SAFETY: SEND_PAIR pins its tail carrier through delivery.
                let tail = unsafe { (*n.send_tail).len } as u64;
                w[..4].copy_from_slice(&[NET_SEND, id, b.len as u64, tail]);
            }
            SEND_SECRET => {
                // A TLS write half; the Secrets themselves go to the call.
                let id = self.socket(request.handle, Kind::Socket)?;
                // SAFETY: SEND_SECRET pins its SendRequest and its marks'
                // bytes through delivery.
                let (secrets, marks) = unsafe {
                    let send = &*request.path.cast::<crate::secret::SendRequest>();
                    (
                        send.designations.len() as u64,
                        (*n.send_tail).len as u64 / 8,
                    )
                };
                w[..5].copy_from_slice(&[NET_SEND_SECRET, id, b.len as u64, secrets, marks]);
            }
            TLS_CONNECT => {
                // The Socket is whole: `tls.connect` takes it before any split.
                let id = self.socket(request.handle, Kind::Socket)?;
                // SAFETY: TLS_CONNECT pins its TlsRequest in `path`.
                let tls = unsafe { &*request.path.cast::<crate::tls::TlsRequest>() };
                w[..5].copy_from_slice(&[
                    NET_HANDSHAKE,
                    id,
                    tls.name.len as u64,
                    tls.alpn.len as u64,
                    u64::from(tls.trust == 0),
                ]);
            }
            TLS_HELLO => {
                // The ClientHello's server name is staged, at most 253 bytes.
                let id = self.socket(request.handle, Kind::Socket)?;
                w[..3].copy_from_slice(&[NET_HELLO, id, 253]);
            }
            TLS_ACCEPT => {
                // SAFETY: TLS_ACCEPT pins its TlsAcceptRequest in `path`.
                let accept =
                    unsafe { &*request.path.cast::<crate::tls_server::TlsAcceptRequest>() };
                let id = self.socket(request.handle, Kind::Socket)?;
                w[..4].copy_from_slice(&[
                    NET_SERVE,
                    id,
                    accept.chain.len as u64,
                    accept.alpn.len as u64,
                ]);
            }
            _ => return None,
        }
        Some(w)
    }
}

pub(super) fn reply_error(vocab: Vocab, call: &PortCall, tag: u64, code: u64) -> Option<IoError> {
    let op = call.words[0];
    // An IoError's tags are 1 to 5, a NetError's 1 to 9; the code is the
    // reply word's low 32 bits.
    let tags = match (vocab, op) {
        (Vocab::Net, SERVED_UNIX_LISTEN) => 10,
        (Vocab::Net, _) => 9,
        _ => 5,
    };
    let ephemeral = (vocab, op, call.words[4]) == (Vocab::Net, NET_LISTEN, 0)
        && tag == u64::from(hirsel_core_net::EPHEMERAL_OUTSIDE_NET_GRANT)
        && code == 0;
    ((1..=tags).contains(&tag) || ephemeral).then_some(IoError {
        tag: tag as u8,
        code: code as u32,
    })
}

/// A caller key's Net port, when `provider` (the socket's recorded one) is
/// a port's.
pub(super) fn net_port(provider: &Provider) -> Option<&Mutex<Port>> {
    if !owns(provider) {
        return None;
    }
    // SAFETY: the run pins every cell until teardown.
    match unsafe { cell(provider.state) } {
        Cell::Port(port) => Some(port),
        _ => None,
    }
}

pub(crate) fn exec_port(provider: &Provider) -> bool {
    net_port(provider).is_some_and(|port| lock(port).vocab == Vocab::Exec)
}

/// `net.split` on a port's socket: both halves are live and the server is
/// asked nothing. Allocates nothing.
pub(crate) fn split_socket(provider: &Provider, key: u64) {
    if let Some(port) = net_port(provider) {
        let mut p = lock(port);
        if let Some(&id) = p.keys.get(&(key & !3))
            && let Some(h) = p.handles.get_mut(&id)
        {
            h.write = Some(State::Live);
        }
    }
}

/// `net.join` on a port socket's live halves: whole again, the server asked
/// nothing. False when the halves are not both live.
pub(crate) fn join_socket(provider: &Provider, key: u64) -> bool {
    let Some(port) = net_port(provider) else {
        return false;
    };
    let mut p = lock(port);
    let Some(&id) = p.keys.get(&(key & !3)) else {
        return false;
    };
    match p.handles.get_mut(&id) {
        Some(h)
            if h.kind == Kind::Socket && h.state == State::Live && h.write == Some(State::Live) =>
        {
            h.write = None;
            true
        }
        _ => false,
    }
}

/// A dropped port socket, half or listener (FAIL-13): queue its `Released`
/// and ready the servers, allocating nothing. Returns whether the caller now
/// holds no part of it, so its key is forgotten.
pub(crate) fn release_socket(provider: &Provider, key: u64) -> bool {
    let Some(port) = net_port(provider) else {
        return true;
    };
    let mut p = lock(port);
    let Some(&id) = p.keys.get(&(key & !3)) else {
        return true;
    };
    let stamp = p.next_id;
    let Some(h) = p.handles.get_mut(&id) else {
        return true;
    };
    let slot = if key & 3 == 2 && h.kind != Kind::PipeWrite {
        h.write.as_mut()
    } else {
        Some(&mut h.state)
    };
    let marked = match slot {
        Some(s) if *s == State::Live => {
            *s = State::Released(stamp);
            true
        }
        _ => false,
    };
    let gone = h.state != State::Live && h.write.is_none_or(|w| w != State::Live);
    if gone {
        p.keys.remove(&(key & !3));
    }
    if marked {
        p.next_id += 1;
        let servers = p.servers.iter().map(|&(t, _)| t).collect();
        drop(p);
        ready_all(servers);
    }
    gone
}

/// Register a narrowed grant only on the generation that authorized its parent.
pub(crate) fn grant_child(provider: &Provider, parent: u64, child: u64) -> bool {
    if retired(provider) {
        return false;
    }
    let Some(port) = net_port(provider) else {
        return true;
    };
    let mut port = lock(port);
    if !port.admits_grant(parent) {
        return false;
    }
    port.grants.insert(child);
    true
}
pub(crate) fn grant_live(provider: &Provider, handle: u64) -> bool {
    !retired(provider) && net_port(provider).is_none_or(|p| lock(p).admits_grant(handle))
}

/// A derived Dir or narrowed grant word lives until its issuing port withdraws.
/// An already committed reply may publish after withdrawal. Its fresh word
/// must immediately resolve to the retired provider.
pub(crate) fn track_derived_word(
    provider: &Provider,
    handle: u64,
    words: impl FnOnce() -> crate::host_file::CellWords,
) {
    if let Some(port) = net_port(provider) {
        let mut p = lock(port);
        if !p.withdrawn && (p.keys.contains_key(&handle) || p.grants.contains(&handle)) {
            p.derived_words.push(words());
        } else {
            drop(p);
            words().retire();
        }
    }
}

pub(crate) fn socket_live(provider: &Provider, key: u64) -> bool {
    net_port(provider).is_none_or(|p| lock(p).keys.contains_key(&(key & !3)))
}

/// NET-18 counts belong to the generation that authorized the connect.
pub(crate) fn port_denials(
    provider: &Provider,
    handle: u64,
    increment: Option<usize>,
) -> Option<Result<[u64; 2], ()>> {
    let mut p = lock(net_port(provider)?);
    if !p.admits_grant(handle) {
        return Some(Err(()));
    }
    if let Some(index) = increment {
        p.denials[index] += 1;
    }
    Some(Ok(p.denials))
}
