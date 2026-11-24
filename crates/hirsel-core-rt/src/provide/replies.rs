use super::*;

/// Whether a reply variant answers the call's operation (core.md, Handles).
pub(super) fn answers(vocab: Vocab, op: u64, reply: u64) -> bool {
    match vocab {
        Vocab::Exec => unreachable!("Exec replies use their own matrix"),
        Vocab::Stream => matches!(reply, R_DONE | R_WRITE_FAILED),
        Vocab::Dir => {
            (reply == R_FAILED && op != DIR_RELEASED)
                || match op {
                    0 | DIR_MKSTEMP => reply == R_FILE,
                    1 | 2 => reply == R_DIR,
                    3 | 14 => reply == R_STAT,
                    9 => reply == R_ENTRIES,
                    10 | 11 | 19 => reply == R_COUNT,
                    12 | 13 => matches!(reply, R_DONE | R_WRITE_FAILED),
                    16 => reply == R_LOCKED,
                    DIR_ZERO_EXTENSION => reply == R_ZERO_EXTENSION,
                    _ => reply == R_DONE,
                }
        }
        Vocab::Net => {
            (reply == R_FAILED && op != NET_RELEASED)
                || match op {
                    NET_CONNECT | NET_ACCEPT => reply == R_SOCKET,
                    NET_LISTEN => reply == R_LISTENER,
                    NET_RESOLVE | NET_RECV | NET_SEND => reply == R_COUNT,
                    NET_HANDSHAKE => reply == R_SECURED,
                    NET_SEND_SECRET | NET_READY => reply == R_DONE,
                    NET_LOCAL_ADDR | NET_PEER_ADDR => reply == R_ADDRESS,
                    _ => false,
                }
        }
    }
}

/// The caller's outcome for a reply that answers its call, or None when the
/// reply breaks the port's rules (the server fails).
pub(super) fn answer(
    p: &Port,
    call: &PortCall,
    unix: Option<u64>,
    tag: u64,
    w: [u64; 6],
) -> Option<Outcome> {
    let op = call.words[0];
    if p.vocab == Vocab::Net && udp::operation(op) {
        return udp::answer(p, call, tag, w);
    }
    if p.vocab == Vocab::Exec {
        let [value, error, code, written] =
            exec::answer(&call.words, call.staged.len() as u64, tag, &w, |id| {
                p.handles.contains_key(&id)
            })?;
        return Some(Outcome {
            value,
            error: IoError {
                tag: error as u8,
                code: code as u32,
            },
            written,
            ..Outcome::default()
        });
    }
    if !unix.map_or_else(
        || answers(p.vocab, op, tag),
        |reply| tag == reply || tag == R_FAILED,
    ) {
        return None;
    }
    if (p.vocab, op, tag) == (Vocab::Dir, DIR_MKSTEMP, R_FILE)
        && !hirsel_core_ids::host_op::valid_temp_suffix(&call.staged)
    {
        return None;
    }
    let io = |tag, code| net::reply_error(p.vocab, call, tag, code);
    let mut out = Outcome::default();
    match tag {
        R_DONE => {
            out.written = match (p.vocab, op) {
                (Vocab::Stream, _) | (Vocab::Dir, 12) => {
                    call.words[2 - (p.vocab == Vocab::Stream) as usize]
                }
                (Vocab::Dir, 13) => call.words[3],
                _ => 0,
            }
        }
        R_FAILED => out.error = io(w[0], w[1])?,
        R_WRITE_FAILED => {
            let len = if op == 13 {
                call.words[3]
            } else {
                call.words[2 - (p.vocab == Vocab::Stream) as usize]
            };
            if w[0] > len {
                return None;
            }
            out.error = io(w[1], w[2])?;
            out.written = w[0];
        }
        R_COUNT if p.vocab == Vocab::Net && op == NET_SEND => {
            // A send's count is at most the bytes it offered.
            if w[0] > call.words[2] + call.words[3] {
                return None;
            }
            out.value = w[0];
        }
        R_COUNT if p.vocab == Vocab::Net && op == NET_RESOLVE => {
            // A resolve's count is its staged records, each well formed and
            // distinct (NOTES 34).
            let records = hirsel_core_net::ip_records(&call.staged)?;
            if w[0] != records.len() as u64 {
                return None;
            }
            out.value = w[0];
        }
        R_COUNT => {
            // A read's count is the bytes it staged; a listing's, its entries.
            if op != 19 && w[0] != call.staged.len() as u64 {
                return None;
            }
            out.value = w[0];
        }
        R_ZERO_EXTENSION if p.vocab == Vocab::Dir => out.value = u64::from(w[0] != 0),
        R_SOCKET | R_LISTENER | R_ADDRESS => {
            let v4 = w[1] == 0 && w[2] == 0 && w[3] <= u64::from(u32::MAX);
            if !(v4 || w[1] == 1) || w[4] > 0xffff {
                return None;
            }
            if tag != R_ADDRESS {
                if w[0] == 0 || p.handles.contains_key(&w[0]) {
                    return None;
                }
                out.value = w[0];
            }
        }
        R_SECURED => {
            // Some(i) names one of the offered protocols; None is (0, 0).
            // SAFETY: the call is unsettled, so its caller's request is pinned.
            let offered = alpn_count(unsafe { field_bytes(p.vocab, call, 2) });
            if !matches!((w[0], w[1]), (0, 0)) && !(w[0] == 1 && w[1] < offered) {
                return None;
            }
            // SAFETY: as above.
            out.value = unsafe { (*call.request).handle };
        }
        R_PACKET => {
            let room = call.words[2];
            if w[1] != u64::from(w[0] > room) || w[0].min(room) != call.staged.len() as u64 {
                return None;
            }
            out.value = w[0];
            out.written = call.staged.len() as u64;
            out.kind = w[1] as u8;
        }
        R_LOCKED => out.value = (w[0] != 0) as u64,
        R_STAT => {
            out.kind = w[0].min(3) as u8;
            out.value = w[1];
            out.written = w[2];
        }
        _ => {
            // A new object: never the root, never an id already in use.
            if w[0] == 0 || p.handles.contains_key(&w[0]) {
                return None;
            }
            out.value = w[0];
        }
    }
    Some(out)
}

/// A Unix socket reply's word and the handle kind it creates.
pub(super) fn unix_reply(reply: UnixReply) -> (u64, Option<Kind>) {
    match reply {
        UnixReply::Created(UnixHandle::Stream) => (R_UNIX_SOCKET, Some(Kind::Socket)),
        UnixReply::Created(UnixHandle::Listener) => (R_UNIX_LISTENING, Some(Kind::UnixListener)),
        UnixReply::Created(UnixHandle::Datagram) => (R_DATAGRAM, Some(Kind::Datagram)),
        UnixReply::Done => (R_DONE, None),
        UnixReply::Packet => (R_PACKET, None),
    }
}
