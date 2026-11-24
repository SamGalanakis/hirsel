use super::*;

#[derive(Clone, Copy)]
pub(super) struct ReadSpace {
    recorded: [u64; 2],
    replaying: [u64; 2],
    at: u64,
}

impl ReadSpace {
    pub(super) fn for_issue(
        recorded: Option<[u64; 2]>,
        replaying: Option<[u64; 2]>,
        at: u64,
    ) -> Result<Option<Self>, Fault> {
        match (recorded, replaying) {
            (None, None) => Ok(None),
            (Some(recorded), Some(replaying)) => Ok(Some(Self {
                recorded,
                replaying,
                at,
            })),
            _ => Err(Fault::Corrupt),
        }
    }

    fn require(self, sizes: [u64; 2]) -> Result<(), Fault> {
        if sizes
            .into_iter()
            .zip(self.recorded)
            .any(|(n, spare)| n > spare)
        {
            return Err(Fault::Corrupt);
        }
        if sizes
            .into_iter()
            .zip(self.replaying)
            .any(|(n, spare)| n > spare)
        {
            return Err(Fault::Diverged(Divergence {
                at: self.at,
                kind: Mismatch::Arg,
            }));
        }
        Ok(())
    }
}

pub(super) unsafe fn replay_out(
    live: &mut Live,
    generation: u32,
    op: u8,
    object: u64,
    r: &mut Request,
    out: Out,
    space: Option<ReadSpace>,
) -> Result<Outcome, Fault> {
    let unix_reply = UnixOperation::describe(op).map(|op| op.reply);
    if let Some(reply) = unix_reply {
        unix::delivery(
            r,
            reply,
            &out,
            live.objects.len() as u64,
            space.map(|room| room.recorded[0]),
        )?;
        if out.status == OK && reply == UnixReply::Packet {
            space.ok_or(Fault::Corrupt)?.require([out.written, 0])?;
        }
    }
    if op == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
        let ordinal = object as usize;
        let part = 1 << (r.mode & 3);
        let resource = live
            .typed_resources
            .get_mut(&ordinal)
            .ok_or(Fault::Corrupt)?;
        if out != Out::default()
            || resource.kind == ResourceKind::Proc
            || resource.parts & part == 0
        {
            return Err(Fault::Corrupt);
        }
        resource.parts &= !part;
        let closed = resource.parts == 0;
        if closed {
            live.typed_resources.remove(&ordinal);
        }
        return Ok(Outcome {
            value: u64::from(closed),
            ..Outcome::default()
        });
    }
    let mut spawn_pipes = [0; 3];
    if matches!(op, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
        if op != EXEC_SPAWN
            && live
                .typed_resources
                .get(&(object as usize))
                .is_none_or(|r| r.kind != ResourceKind::Proc)
        {
            return Err(Fault::Corrupt);
        }
        spawn_pipes = exec::delivery(r, &out, live.objects.len() as u64)?;
    }
    if op == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
        if out != Out::default()
            || live
                .typed_resources
                .get(&(object as usize))
                .is_none_or(|r| r.kind != ResourceKind::Proc)
        {
            return Err(Fault::Corrupt);
        }
        live.typed_resources.remove(&(object as usize));
        return Ok(Outcome::default());
    }
    let mut o = Outcome {
        value: out.value,
        error: IoError {
            tag: out.error.0,
            code: u32::try_from(out.error.1).map_err(|_| Fault::Corrupt)?,
        },
        written: out.written,
        status: 0,
        kind: 0,
    };
    match out.status {
        OK => {}
        ERROR if out.error.0 != 0 => {}
        ABORTED | TIMED_OUT => {
            if op == EXEC_WAIT {
                o.error.tag = if out.status == ABORTED { 3 } else { 2 };
            } else if waiting_io(op) {
                o.error.tag = if out.status == ABORTED { 7 } else { 6 };
            }
        }
        _ => return Err(Fault::Corrupt),
    }
    let ok = out.status == OK;
    if clock(op) {
        let prev = live.clocks.get(&(op, object)).copied().unwrap_or(0);
        o.value = prev.wrapping_add(unzz(out.value) as u64);
        live.clocks.insert((op, object), o.value);
    }
    let unix_kind = unix_reply.and_then(unix::kind);
    if ok && (creates_object(op) || unix_kind.is_some()) {
        if out.value != live.objects.len() as u64 + 1 {
            return Err(Fault::Corrupt);
        }
        live.objects.push(Object {
            inner: None,
            open_resource: false,
        });
        o.value = handle(generation, live.objects.len());
        if let Some(kind) = unix_kind {
            live.typed_resources
                .insert(ordinal_of(o.value), Resource::new(kind, 0));
        }
    }
    if ok && op == EXEC_SPAWN {
        live.typed_resources
            .insert(live.objects.len(), Resource::new(ResourceKind::Proc, 0));
        // SAFETY: every ordinal and the whole pipe plan passed delivery validation.
        let call = unsafe {
            &mut (*r
                .path
                .cast_mut()
                .cast::<crate::exec_runtime::Call<crate::exec_runtime::SpawnCall>>())
            .data
        };
        for (i, pipe) in spawn_pipes.into_iter().enumerate() {
            if pipe == 0 {
                continue;
            }
            let part = if i == 0 { 2 } else { 1 };
            live.objects.push(Object {
                inner: None,
                open_resource: false,
            });
            live.typed_resources
                .insert(pipe as usize, Resource::new(ResourceKind::Pipe, part));
            call.pipes[i] = (pipe << 2) | u64::from(part);
        }
    }
    if matches!(op, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
        return Ok(o);
    }
    if unix_reply == Some(UnixReply::Packet) {
        if ok {
            let (flag, bytes) = out.payload.split_first().unwrap();
            if !bytes.is_empty() {
                // SAFETY: Unix delivery validation proved the byte count fits.
                unsafe {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        r.buffer.data.add(r.buffer.len),
                        bytes.len(),
                    )
                };
                r.buffer.len += bytes.len();
            }
            o.kind = *flag;
        }
        return Ok(o);
    }
    if matches!(op, LISTEN | ACCEPT | LOCAL_ADDR | PEER_ADDR) {
        if ok {
            // SAFETY: this request pins the writable address through delivery.
            let network = unsafe { &mut *r.path.cast_mut().cast::<crate::net::NetworkRequest>() };
            let mut bytes = Cursor::new(&out.payload);
            network.address = crate::net::Address {
                family: bytes.u8()?,
                hi: u64::from_le_bytes(bytes.fixed()?),
                lo: u64::from_le_bytes(bytes.fixed()?),
                port: u16::from_le_bytes(bytes.fixed()?),
            };
        }
    } else if matches!(op, STAT | STAT_FILE) {
        if ok {
            let [kind, time @ ..] = out.payload.as_slice() else {
                return Err(Fault::Corrupt);
            };
            if *kind > 3 {
                return Err(Fault::Corrupt);
            }
            o.kind = *kind;
            o.written = u64::from_le_bytes(time.try_into().map_err(|_| Fault::Corrupt)?);
        } else if !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
    } else if op == TLS_CONNECT {
        if ok {
            let (protocol, digest, binding) = tls_connect_payload(&out.payload)?;
            // SAFETY: TLS_CONNECT pins the offer and its output buffers.
            unsafe {
                let tls = &*r.path.cast::<crate::tls::TlsRequest>();
                let buffer = &mut *tls.protocol_out;
                if protocol.len() > buffer.capacity
                    || (!protocol.is_empty() && !tls.offers(protocol))
                {
                    return Err(Fault::Corrupt);
                }
                if !protocol.is_empty() {
                    ptr::copy_nonoverlapping(protocol.as_ptr(), buffer.data, protocol.len());
                }
                buffer.len = protocol.len();
                *tls.peer_out = *digest;
                let out = &mut *tls.binding_out;
                if binding.len() > out.capacity {
                    return Err(Fault::Corrupt);
                }
                ptr::copy_nonoverlapping(binding.as_ptr(), out.data, binding.len());
                out.len = binding.len();
            }
        }
    } else if op == RESOLVE {
        if ok {
            let count = usize::try_from(out.value).map_err(|_| Fault::Corrupt)?;
            if count.checked_mul(hirsel_core_net::IP_RECORD_LEN) != Some(out.payload.len()) {
                return Err(Fault::Corrupt);
            }
            space.ok_or(Fault::Corrupt)?.require([out.value, 0])?;
            let addresses = hirsel_core_net::ip_records(&out.payload).ok_or(Fault::Corrupt)?;
            // SAFETY: RESOLVE pins the list with at least count spare slots.
            unsafe { crate::resolve::append(&mut r.buffer, &addresses) };
        }
    } else if op == MEDIA_P256_KEY {
        let point: &[u8; hirsel_core_net::p256_key::PUBLIC_LEN] = out
            .payload
            .as_slice()
            .try_into()
            .map_err(|_| Fault::Corrupt)?;
        if !ok || out.value != live.schedule.events || point[0] != 4 {
            return Err(Fault::Corrupt);
        }
        space
            .ok_or(Fault::Corrupt)?
            .require([point.len() as u64, 0])?;
        // SAFETY: the delivery has the exact point size and fits the writable spare.
        unsafe { crate::secret::p256_generation::append_point(&mut r.buffer, point) };
    } else if op == SECRET_KDF {
        if ok {
            if out.value != live.schedule.events {
                return Err(Fault::Corrupt);
            }
        } else if r.mode != crate::secret::kdf::ARGON2ID_MODE
            || out.error != (1, 0)
            || out.value != 0
        {
            return Err(Fault::Corrupt);
        }
    } else if op == CRED_FETCH && ok {
        if out.value != live.schedule.events {
            return Err(Fault::Corrupt);
        }
        let mut c = Cursor::new(&out.payload);
        let key_id = c.bytes().map_err(|_| Fault::Corrupt)?;
        let token = c.bytes().map_err(|_| Fault::Corrupt)?;
        let expires_ns = u64::from_le_bytes(c.fixed().map_err(|_| Fault::Corrupt)?);
        if !c.done() {
            return Err(Fault::Corrupt);
        }
        space
            .ok_or(Fault::Corrupt)?
            .require([key_id.len() as u64, token.len() as u64])?;
        // SAFETY: CRED_FETCH pins its adapter extension through delivery.
        let cred = unsafe { &mut *r.path.cast_mut().cast::<crate::cred::CredRequest>() };
        cred.fetched = Some(crate::cred::Fetched {
            key_id: key_id.to_vec().into(),
            token: token.to_vec().into(),
            secret: None,
            expires_ns,
        });
    } else if op == SECRET_MAC && crate::secret::key::is_key_mode(r.mode) {
        // Key operations record appended bytes, or just an outcome for unwrap.
        let produced = r.mode != crate::secret::key::KEY_UNWRAP;
        if if ok {
            !produced && (out.value != 0 || !out.payload.is_empty())
                || produced && out.value != out.payload.len() as u64
        } else {
            !(1..=4).contains(&out.error.0) || !out.payload.is_empty()
        } {
            return Err(Fault::Corrupt);
        }
        if out.error.0 == 4 {
            o.written = space.ok_or(Fault::Corrupt)?.recorded[0];
        }
        if ok && produced {
            space.ok_or(Fault::Corrupt)?.require([out.value, 0])?;
            if !out.payload.is_empty() {
                // SAFETY: the nonempty payload fits the carrier's spare capacity.
                unsafe {
                    ptr::copy_nonoverlapping(
                        out.payload.as_ptr(),
                        r.buffer.data.add(r.buffer.len),
                        out.payload.len(),
                    );
                }
                r.buffer.len += out.payload.len();
            }
        }
    } else if op == SECRET_MAC {
        // A signing adapter pins an inline destination of exactly the tag length.
        // A verification adapter supplies an empty output carrier.
        let tag_len = r.buffer.capacity;
        if !ok {
            if !(crate::secret::is_jws_mac_mode(r.mode)
                && out.status == ERROR
                && out.error == (crate::secret::SHORT_JWS_KEY, 0)
                && out.value == 0
                && out.payload.is_empty())
            {
                return Err(Fault::Corrupt);
            }
        } else if tag_len != 0 {
            if out.value != 0 || out.payload.len() != tag_len {
                return Err(Fault::Corrupt);
            }
            // SAFETY: the validated payload fits the adapter's pinned destination.
            unsafe { ptr::copy_nonoverlapping(out.payload.as_ptr(), r.buffer.data, tag_len) };
            r.buffer.len = tag_len;
        } else if out.value > 1 || !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
    } else if carries_payload(op) {
        if op == NEXT_ENTRIES
            && (!valid_entries(&out.payload, out.value) || (!ok && !out.payload.is_empty()))
        {
            return Err(Fault::Corrupt);
        }
        if op == NEXT_ENTRIES {
            // The tape uses the byte extent to join continuation frames.
            o.written = 0;
        }
        if op == READ_AT && !ok && !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
        if let Some(space) = space {
            space.require([out.payload.len() as u64, 0])?;
        } else if out.payload.len() > r.buffer.capacity.saturating_sub(r.buffer.len) {
            return Err(Fault::Corrupt);
        }
        if !out.payload.is_empty() {
            // SAFETY: the payload fits the carrier's spare capacity.
            unsafe {
                ptr::copy_nonoverlapping(
                    out.payload.as_ptr(),
                    r.buffer.data.add(r.buffer.len),
                    out.payload.len(),
                );
            }
            r.buffer.len += out.payload.len();
        }
    } else if op == WRITE_AT {
        if out.written > r.buffer.len as u64 || (ok && out.written != r.buffer.len as u64) {
            return Err(Fault::Corrupt);
        }
    } else if op == RECV_READY {
        // Readiness carries no count: only its status and error.
        if out.value != 0 || out.written != 0 {
            return Err(Fault::Corrupt);
        }
    } else if op == SEND_SECRET {
        if out.value != 0 || (ok && out.written != 0) {
            return Err(Fault::Corrupt);
        }
    } else if op == SCRAM_VERIFY {
        if !ok || out.value > 1 {
            return Err(Fault::Corrupt);
        }
    } else if op == SEND_PAIR && ok {
        // SAFETY: the send adapter pins the tail carrier through delivery.
        let tail = unsafe { &*(*r.path.cast::<crate::net::NetworkRequest>()).send_tail };
        if out.value > (r.buffer.len as u64).saturating_add(tail.len as u64) {
            return Err(Fault::Corrupt);
        }
    }
    Ok(o)
}

/// Refuse malformed record streams before they reach a caller's Bytes carrier.
fn valid_entries(mut bytes: &[u8], count: u64) -> bool {
    let mut records = 0;
    while let Some(([kind, lo, hi], rest)) = bytes.split_first_chunk::<3>() {
        let n = u16::from_le_bytes([*lo, *hi]) as usize;
        if *kind > 3 || !(1..=255).contains(&n) {
            return false;
        }
        let Some((name, tail)) = rest.split_at_checked(n) else {
            return false;
        };
        if matches!(name, b"." | b"..") || name.iter().any(|&b| b == 0 || b == b'/') {
            return false;
        }
        bytes = tail;
        records += 1;
    }
    bytes.is_empty() && records == count
}
