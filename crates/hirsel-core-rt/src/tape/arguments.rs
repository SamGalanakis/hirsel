//! Canonical public operands for the tape issue frame.
use super::*;

unsafe fn operands(r: &Request) -> Args {
    // SAFETY: the request's operands are pinned for its issue.
    unsafe {
        if matches!(r.operation, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
            return exec::canonical(r);
        }
        if r.operation == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
            return Args::of(&[0]);
        }
        if r.operation == CLOSE && r.mode == crate::udp_net::record::CLOSE_MODE {
            return Args::of(crate::udp_net::record::CLOSE_ARGS);
        }
        if r.operation == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
            return Args::of(&[r.mode & 3]);
        }
        if let Some(op) = UnixOperation::describe(r.operation) {
            return unix::canonical(r, op);
        }
        if udp_operation(r.operation) {
            return crate::udp_net::record::arguments(r);
        }
        if socket_op(r.operation) {
            return socket_args(r);
        }
        if r.operation == STDIN_READ {
            let read = &*r.path.cast::<crate::stdin::ReadRequest>();
            let mut args = Vec::new();
            if let Some((_, clock, at)) = read.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
            return Args::of(&args);
        }
        if r.operation == STDIN_SECRET {
            // The deadline and the policy text; never the line or its length.
            let read = &*r.path.cast::<crate::stdin_secret::SecretRequest>();
            let mut args = Vec::new();
            if let Some((_, clock, at)) = read.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
            put_varint(&mut args, read.policy.len() as u64);
            args.extend_from_slice(read.policy.as_bytes());
            return Args::of(&args);
        }
        if r.operation == SCRAM_VERIFY {
            return Args::of(buffer_bytes(&r.buffer));
        }
        if r.operation == CRED_FETCH {
            // The grant is the frame's object; never the source or its replies.
            let cred = &*r.path.cast::<crate::cred::CredRequest>();
            let mut args = Vec::with_capacity(32);
            if let Some((_, clock, at)) = cred.network.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
            return Args::of(&args);
        }
        if r.operation == SECRET_KDF {
            return secret::kdf_args(r);
        }
        if r.operation == MEDIA_KEY_SHARE {
            let call = &*r.path.cast::<crate::secret::key_share::Generation>();
            let mut encoded = vec![call.group as u8];
            put_varint(&mut encoded, call.random);
            put_varint(&mut encoded, (r.buffer.capacity - r.buffer.len) as u64);
            return Args::of(&encoded);
        }
        if r.operation == MEDIA_P256_KEY {
            let call = &*r.path.cast::<crate::secret::p256_generation::Generation>();
            let mut random = Vec::new();
            put_varint(&mut random, call.random);
            put_varint(&mut random, (r.buffer.capacity - r.buffer.len) as u64);
            return Args::of_parts(&[&random, call.policy.as_bytes()]);
        }
        if r.operation == MEDIA_CAPTURE {
            return crate::media::capture::canonical(r);
        }
        if r.operation == MEDIA_AGREEMENT {
            let call = &*r.path.cast::<crate::secret::agreement::Call<'_>>();
            return Args::of(call.canonical);
        }
        if r.operation == SECRET_MAC && crate::secret::key::is_key_mode(r.mode) {
            return secret::key_args(r);
        }
        if r.operation == SECRET_MAC {
            let mac = &*r.path.cast::<crate::secret::MacRequest<'_>>();
            let policy = mac.secret.policy.text();
            let mut head = vec![r.mode];
            mac.secret.origin.put(&mut head);
            put_varint(&mut head, policy.len() as u64);
            put_varint(&mut head, mac.data.len() as u64);
            return Args::of_parts(&[
                &head,
                policy.as_bytes(),
                mac.data,
                mac.tag.unwrap_or_default(),
            ]);
        }
        let path = if r.path_len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(r.path, r.path_len)
        };
        let bytes = if r.buffer.len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(r.buffer.data, r.buffer.len)
        };
        Args::of(&match r.operation {
            OPEN => {
                let mut a = vec![r.mode];
                a.extend_from_slice(path);
                a
            }
            READ_AT | ZERO_AT => path.to_vec(),
            WRITE_AT => return Args::of_parts(&[path, bytes]),
            WRITE | STDOUT | STDERR | MKDTEMP | MKSTEMP => bytes.to_vec(),
            MKDIR | REMOVE | REMOVE_TREE | RENAME | OPEN_DIR | ENTRIES | STAT => path.to_vec(),
            SLEEP_UNTIL | SLEEP_UNTIL_WALL => bytes.to_vec(),
            _ => vec![],
        })
    }
}

pub(super) unsafe fn canonical(r: &Request) -> Args {
    // SAFETY: the request pins all of its operands and output carriers.
    let arguments = unsafe { operands(r) };
    match r.operation {
        CRED_FETCH => {
            // SAFETY: the credential adapter pins this extension for the request.
            let cred = unsafe { &*r.path.cast::<crate::cred::CredRequest>() };
            arguments.buffered(cred.spares)
        }
        RESOLVE => arguments.buffered([crate::resolve::spare(&r.buffer) as u64, 0]),
        READ | READ_AT | RANDOM | NEXT_ENTRIES | RECV_BOUNDED | DATAGRAM_RECV | STDIN_READ
        | MKSTEMP | MEDIA_P256_KEY | MEDIA_KEY_SHARE => {
            arguments.buffered([r.buffer.capacity.saturating_sub(r.buffer.len) as u64, 0])
        }
        SECRET_MAC
            if matches!(
                r.mode,
                crate::secret::key::KEY_WRAP
                    | crate::secret::key::KEY_SIGN
                    | crate::secret::key::KEY_AEAD_SEAL
                    | crate::secret::key::KEY_AEAD_OPEN
            ) =>
        {
            arguments.buffered([r.buffer.capacity.saturating_sub(r.buffer.len) as u64, 0])
        }
        _ => arguments,
    }
}
