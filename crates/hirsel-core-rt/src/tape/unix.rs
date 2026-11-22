//! Path and packet rows for Unix sockets. Endpoints are logical root/path pairs.
use super::{Args, Fault, Out, Request, ResourceKind, ordinal_of, put_varint};
use hirsel_core_ids::host_op::*;

pub(super) fn kind(operation: u8) -> Option<ResourceKind> {
    match operation {
        UNIX_CONNECT | UNIX_ACCEPT => Some(ResourceKind::UnixStream),
        UNIX_LISTEN => Some(ResourceKind::UnixListener),
        DATAGRAM_OPEN | DATAGRAM_BIND => Some(ResourceKind::Datagram),
        _ => None,
    }
}

pub(super) fn can_park(operation: u8) -> bool {
    matches!(
        operation,
        UNIX_CONNECT | UNIX_ACCEPT | DATAGRAM_SEND | DATAGRAM_SEND_TO | DATAGRAM_RECV
    )
}

pub(super) unsafe fn canonical(r: &Request) -> Args {
    // SAFETY: each Unix operation pins this extension and its buffer.
    unsafe {
        let req = &*r.path.cast::<crate::unix_net::UnixRequest>();
        let mut args = Vec::new();
        if matches!(
            r.operation,
            UNIX_CONNECT | UNIX_LISTEN | DATAGRAM_BIND | DATAGRAM_CONNECT | DATAGRAM_SEND_TO
        ) {
            for part in [req.root.as_bytes(), req.path.as_bytes()] {
                put_varint(&mut args, part.len() as u64);
                args.extend_from_slice(part);
            }
        }
        if r.operation == UNIX_LISTEN {
            args.extend_from_slice(&req.backlog.to_le_bytes());
        }
        if can_park(r.operation) {
            if let Some((_, clock, at)) = req.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
        }
        if r.operation == DATAGRAM_RECV {
            args.extend_from_slice(
                &(r.buffer.capacity.saturating_sub(r.buffer.len) as u64).to_le_bytes(),
            );
        }
        if matches!(r.operation, DATAGRAM_SEND | DATAGRAM_SEND_TO) {
            let bytes = super::buffer_bytes(&r.buffer);
            put_varint(&mut args, bytes.len() as u64);
            return Args::of_parts(&[&args, bytes]);
        }
        Args::of(&args)
    }
}

pub(super) fn delivery(r: &Request, out: &Out, objects: u64) -> Result<(), Fault> {
    use super::{ABORTED, ERROR, OK, TIMED_OUT};
    let op = r.operation;
    if out.status == OK {
        if out.error != (0, 0) {
            return Err(Fault::Corrupt);
        }
        if op == DATAGRAM_RECV {
            let space = r.buffer.capacity.saturating_sub(r.buffer.len) as u64;
            let count = space.min(out.value);
            let Some((&flag, payload)) = out.payload.split_first() else {
                return Err(Fault::Corrupt);
            };
            if out.value > isize::MAX as u64
                || flag != u8::from(out.value > space)
                || out.written != count
                || payload.len() as u64 != count
            {
                return Err(Fault::Corrupt);
            }
        } else {
            let expected = kind(op).map_or(0, |_| objects + 1);
            if out.value != expected || out.written != 0 || !out.payload.is_empty() {
                return Err(Fault::Corrupt);
            }
        }
    } else {
        if out.value != 0 || out.written != 0 || !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
        match (out.status, out.error) {
            (ABORTED | TIMED_OUT, (0, 0)) if can_park(op) => {}
            (ERROR, (1..=3, 0) | (8, 1..=4095)) => {}
            (ERROR, (4, 0)) if matches!(op, UNIX_LISTEN | DATAGRAM_BIND) => {}
            (ERROR, (10, 1..=4095)) if op == UNIX_LISTEN => {}
            _ => return Err(Fault::Corrupt),
        }
    }
    Ok(())
}
