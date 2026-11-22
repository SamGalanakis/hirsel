//! Path and packet rows for Unix sockets. Endpoints are logical root/path pairs.
use super::{Args, Fault, Out, Request, ResourceKind, ordinal_of, put_varint};
use crate::unix_net::{UnixField, UnixHandle, UnixOperation, UnixReply, UnixRequest};
use hirsel_core_ids::host_op::*;

pub(super) fn kind(reply: UnixReply) -> Option<ResourceKind> {
    match reply {
        UnixReply::Created(UnixHandle::Stream) => Some(ResourceKind::UnixStream),
        UnixReply::Created(UnixHandle::Listener) => Some(ResourceKind::UnixListener),
        UnixReply::Created(UnixHandle::Datagram) => Some(ResourceKind::Datagram),
        UnixReply::Done | UnixReply::Packet => None,
    }
}

pub(super) fn can_park(operation: u8) -> bool {
    matches!(
        operation,
        UNIX_CONNECT | UNIX_ACCEPT | DATAGRAM_SEND | DATAGRAM_SEND_TO | DATAGRAM_RECV
    )
}

pub(super) unsafe fn canonical(r: &Request, op: UnixOperation) -> Args {
    // SAFETY: each Unix operation pins this extension and its buffer.
    unsafe {
        let req = &*r.path.cast::<UnixRequest>();
        let mut args = Vec::new();
        for field in op.fields {
            let part = match field {
                UnixField::Root(_) => Some(req.root.as_bytes()),
                UnixField::Path => Some(req.path.as_bytes()),
                UnixField::Backlog => {
                    args.extend_from_slice(&req.backlog.to_le_bytes());
                    None
                }
                _ => None,
            };
            if let Some(part) = part {
                put_varint(&mut args, part.len() as u64);
                args.extend_from_slice(part);
            }
        }
        if can_park(r.operation) {
            if let Some((_, clock, at)) = req.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
        }
        match op.fields.last() {
            Some(UnixField::Room) => args.extend_from_slice(
                &(r.buffer.capacity.saturating_sub(r.buffer.len) as u64).to_le_bytes(),
            ),
            Some(UnixField::Bytes) => {
                let bytes = super::buffer_bytes(&r.buffer);
                put_varint(&mut args, bytes.len() as u64);
                return Args::of_parts(&[&args, bytes]);
            }
            _ => {}
        }
        Args::of(&args)
    }
}

pub(super) fn delivery(
    r: &Request,
    reply: UnixReply,
    out: &Out,
    objects: u64,
) -> Result<(), Fault> {
    use super::{ABORTED, ERROR, OK, TIMED_OUT};
    let op = r.operation;
    if out.status == OK {
        if out.error != (0, 0) {
            return Err(Fault::Corrupt);
        }
        if reply == UnixReply::Packet {
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
            let expected = if matches!(reply, UnixReply::Created(_)) {
                objects + 1
            } else {
                0
            };
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
