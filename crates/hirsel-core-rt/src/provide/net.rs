use super::*;

impl Port {
    pub(super) fn net_words(&self, request: &Request) -> Option<[u64; 9]> {
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
