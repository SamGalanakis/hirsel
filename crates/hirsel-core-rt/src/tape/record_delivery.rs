use super::*;

/// Normalize a provider outcome and assign newly created object ordinals at
/// production, before another task can observe the completion.
pub(super) unsafe fn record_out(
    live: &mut Live,
    generation: u32,
    r: &Request,
    before: usize,
    inner: Inner,
    mut o: Outcome,
) -> (Out, Outcome, Delivery) {
    let ordinal = ordinal_of(r.handle);
    let ok = o.status == 0 && o.error == IoError::OK;
    if ok && r.operation == CRED_FETCH {
        // The fetched key is designated by this delivery's tape position.
        o.value = live.schedule.events + 1;
    }
    let mut out = Out {
        status: if r.operation == EXEC_WAIT {
            match o.error.tag {
                0 => OK,
                2 => TIMED_OUT,
                3 => ABORTED,
                _ => ERROR,
            }
        } else if waiting_io(r.operation) {
            waiting_io_status(o.error.tag)
        } else if o.error == IoError::OK {
            OK
        } else {
            ERROR
        },
        value: o.value,
        error: (o.error.tag, u64::from(o.error.code)),
        written: o.written,
        payload: vec![],
    };
    let mut answer = o;
    if r.operation == MEDIA_AGREEMENT {
        // The ordinal names the schedule; the frame's value stays the verdict.
        answer.written = live.schedule.events + 1;
    }
    let mut created = None;
    if ok && udp_operation(r.operation) {
        let mut handles = Vec::new();
        if crate::udp_net::record::creates(r.operation) {
            out.value = live.objects.len() as u64 + 1;
            // SAFETY: the successful UDP creation initialized every promised handle.
            for key in unsafe { crate::udp_net::record::created(r, o.value) } {
                live.objects.push(Object {
                    inner: Some(Inner {
                        provider: inner.provider,
                        handle: key,
                    }),
                    open_resource: false,
                });
                handles.push(handle(generation, live.objects.len()));
                live.typed_resources
                    .insert(live.objects.len(), Resource::new(ResourceKind::Udp, 0));
            }
        }
        // SAFETY: the request pins its successful UDP delivery.
        unsafe { crate::udp_net::record::record(r, &mut out, handles) };
    }
    let unix_reply = UnixOperation::describe(r.operation).map(|op| op.reply);
    let unix_kind = unix_reply.and_then(unix::kind);
    if ok && (creates_object(r.operation) || unix_kind.is_some()) {
        live.objects.push(Object {
            inner: Some(Inner {
                provider: inner.provider,
                handle: o.value,
            }),
            open_resource: matches!(r.operation, OPEN | ENTRIES | MKSTEMP),
        });
        out.value = live.objects.len() as u64;
        answer.value = handle(generation, live.objects.len());
        created = Some(answer.value);
        if let Some(kind) = unix_kind {
            live.typed_resources
                .insert(live.objects.len(), Resource::new(kind, 0));
        }
    }
    if ok && r.operation == EXEC_SPAWN {
        live.typed_resources
            .insert(live.objects.len(), Resource::new(ResourceKind::Proc, 0));
        // SAFETY: spawn pins its private output slots through delivery.
        let call = unsafe {
            &mut (*r
                .path
                .cast_mut()
                .cast::<crate::exec_runtime::Call<crate::exec_runtime::SpawnCall>>())
            .data
        };
        for (i, pipe) in call.pipes.iter_mut().enumerate() {
            if *pipe == 0 {
                put_varint(&mut out.payload, 0);
                continue;
            }
            let part = if i == 0 { 2 } else { 1 };
            live.objects.push(Object {
                inner: Some(Inner {
                    provider: inner.provider,
                    handle: *pipe & !3,
                }),
                open_resource: false,
            });
            live.typed_resources
                .insert(live.objects.len(), Resource::new(ResourceKind::Pipe, part));
            put_varint(&mut out.payload, live.objects.len() as u64);
            *pipe = (live.objects.len() as u64) << 2 | u64::from(part);
        }
    }
    if ok && r.operation == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
        live.typed_resources.remove(&ordinal);
    }
    if ok && r.operation == CLOSE && r.mode == crate::udp_net::record::CLOSE_MODE {
        live.typed_resources.remove(&ordinal);
    }
    if ok && r.operation == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
        let entry = live.typed_resources.get_mut(&ordinal).unwrap();
        entry.parts &= !(1 << (r.mode & 3));
        if entry.parts == 0 {
            live.typed_resources.remove(&ordinal);
        }
        out.value = 0;
    }
    if ok && matches!(r.operation, CLOSE | CLOSE_ENTRIES) {
        live.objects[ordinal - 1].open_resource = false;
    }
    if clock(r.operation) {
        let prev = live
            .clocks
            .insert((r.operation, ordinal as u64), o.value)
            .unwrap_or(0);
        out.value = zz(o.value.wrapping_sub(prev) as i64);
    }
    if ok && matches!(r.operation, LISTEN | ACCEPT | LOCAL_ADDR | PEER_ADDR) {
        // SAFETY: the source wrote the address into the pinned network operand.
        let network = unsafe { &*r.path.cast::<crate::net::NetworkRequest>() };
        out.payload = address_bytes(network.address).to_vec();
    } else if ok && matches!(r.operation, STAT | STAT_FILE) {
        out.payload.push(o.kind);
        out.payload.extend_from_slice(&o.written.to_le_bytes());
    } else if ok && unix_reply == Some(UnixReply::Packet) {
        out.payload.push(o.kind);
        if r.buffer.len > before {
            // SAFETY: receive initialized exactly its appended bytes.
            out.payload.extend(unsafe {
                std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before)
            });
        }
    } else if ok && r.operation == RESOLVE {
        // SAFETY: the resolve provider initialized these Ip elements in spare.
        for i in before..r.buffer.len {
            let ip = unsafe { r.buffer.data.cast::<hirsel_core_net::Ip>().add(i).read() };
            out.payload
                .extend_from_slice(&hirsel_core_net::ip_record(std::net::IpAddr::from(ip)));
        }
    } else if ok && r.operation == CRED_FETCH {
        // Received plaintext only: the key id and token. The key stays sealed.
        // SAFETY: the provider set the pinned extension's success.
        let fetched = unsafe {
            (*r.path.cast::<crate::cred::CredRequest>())
                .fetched
                .as_ref()
        };
        let fetched = fetched.unwrap();
        put_varint(&mut out.payload, fetched.key_id.len() as u64);
        out.payload.extend_from_slice(&fetched.key_id);
        put_varint(&mut out.payload, fetched.token.len() as u64);
        out.payload.extend_from_slice(&fetched.token);
        out.payload
            .extend_from_slice(&fetched.expires_ns.to_le_bytes());
    } else if ok && r.operation == TLS_CONNECT {
        // SAFETY: the handshake pins both outputs through delivery.
        unsafe {
            let tls = &*r.path.cast::<crate::tls::TlsRequest>();
            let protocol = buffer_bytes(&*tls.protocol_out);
            let binding = tls.binding();
            out.payload.push(protocol.len() as u8);
            out.payload.extend_from_slice(protocol);
            out.payload.extend_from_slice(&*tls.peer_out);
            out.payload.push(binding.len() as u8);
            out.payload.extend_from_slice(binding);
        }
    } else if ok
        && carries_payload(r.operation)
        && r.buffer.len.saturating_sub(before) > 256
        && order_only()
    {
        // mc-units-untied order-only variant (not replayable): a payload over
        // 256 bytes is recorded as its length only (`value`), no bytes, no digest.
        TAPE_COUNTS[3].fetch_add(
            (r.buffer.len - before) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    } else if ok && carries_payload(r.operation) && r.buffer.len > before {
        // SAFETY: source initialized these bytes in the request's spare carrier.
        out.payload =
            unsafe { std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before) }
                .to_vec();
        TAPE_COUNTS[2].fetch_add(
            out.payload.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    if r.operation == NEXT_ENTRIES {
        out.written = out.payload.len() as u64;
    }
    (
        out,
        answer,
        Delivery {
            inner,
            outcome: o,
            created,
            refused: false,
        },
    )
}
