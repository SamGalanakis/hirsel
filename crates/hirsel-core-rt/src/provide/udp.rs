use super::*;
use crate::{
    net::Address,
    udp_net::served::{self, Retirement},
};

const OPEN: u64 = 21;
const BIND: u64 = 22;
pub(super) const RECV: u64 = 23;
pub(super) const SEND: u64 = 24;
const LOCAL: u64 = 25;
const SOCKETS: u64 = 17;
const PACKETS: u64 = 18;

pub(super) fn operation(op: u64) -> bool {
    (OPEN..=LOCAL).contains(&op)
}

pub(super) fn packet_charge(count: usize) -> u64 {
    48 * count as u64
}

impl Port {
    pub(super) fn udp_words(&self, r: &Request) -> Option<[u64; 9]> {
        // SAFETY: the provider only admits this row for a typed UDP request.
        let input = unsafe { served::inputs(r) };
        let a = input.endpoint;
        let mut words = [0; 9];
        match r.operation {
            UDP_OPEN => {
                words[..4].copy_from_slice(&[
                    OPEN,
                    u64::from(a.family),
                    input.slots,
                    input.slot_bytes,
                ]);
            }
            UDP_BIND | UDP_BIND_GROUP => {
                words = [
                    BIND,
                    u64::from(a.family),
                    a.hi,
                    a.lo,
                    u64::from(a.port),
                    input.slots,
                    input.slot_bytes,
                    input.members,
                    input.steer,
                ];
            }
            UDP_RECV | UDP_SEND | UDP_LOCAL_ADDR => {
                let id = self.socket(r.handle, Kind::Udp)?;
                let op = match r.operation {
                    UDP_RECV => RECV,
                    UDP_SEND => SEND,
                    _ => LOCAL,
                };
                words[..4].copy_from_slice(&[op, id, input.room, input.entries]);
            }
            _ => return None,
        }
        Some(words)
    }
}

fn address(w: [u64; 6]) -> Option<Address> {
    let v4 = w[1] == 0 && w[2] == 0 && w[3] <= u64::from(u32::MAX);
    if !(v4 || w[1] == 1) {
        return None;
    }
    Some(Address {
        family: w[1] as u8,
        hi: w[2],
        lo: w[3],
        port: u16::try_from(w[4]).ok()?,
    })
}

pub(super) fn answer(p: &Port, call: &PortCall, tag: u64, w: [u64; 6]) -> Option<Outcome> {
    let op = call.words[0];
    let mut out = Outcome::default();
    match (op, tag) {
        (_, R_FAILED) => {
            let ordinary = (1..=8).contains(&w[0]);
            let oversize = op == SEND && w[0] == 12 && w[1] == 0;
            if !(ordinary || oversize) || (w[0] != 8 && w[1] != 0) {
                return None;
            }
            out.error = IoError {
                tag: w[0] as u8,
                code: u32::try_from(w[1]).ok()?,
            };
        }
        (OPEN | BIND, SOCKETS) => {
            let actual = address(w)?;
            let count = if op == OPEN { 1 } else { call.words[7] };
            // SAFETY: the unsettled call pins the endpoint used for this admission.
            let requested = unsafe { served::inputs(&*call.request).endpoint };
            if !crate::udp_net::valid_creation(
                requested,
                actual,
                op == BIND,
                w[0],
                w[5],
                count,
                |range| p.handles.range(range).next().is_none(),
            ) {
                return None;
            }
            out.value = w[0];
        }
        (RECV, PACKETS) => {
            // SAFETY: answer holds the port lock and the live call pins its request.
            let valid =
                unsafe { served::valid(&*call.request, &call.staged_packets, call.staged.len()) };
            if !valid || w[0] != call.staged_packets.len() as u64 {
                return None;
            }
            out.value = w[0];
        }
        (SEND, R_COUNT) if w[0] <= call.words[3] => out.value = w[0],
        (LOCAL, R_ADDRESS) => {
            let a = address(w)?;
            // SAFETY: the unsettled call pins its request and expected family.
            let family = unsafe { served::inputs(&*call.request).endpoint.family };
            if family != a.family || a.port == 0 {
                return None;
            }
        }
        _ => return None,
    }
    Some(out)
}

pub(super) fn reply(
    mut p: MutexGuard<'_, Port>,
    id: u64,
    tag: u64,
    w: [u64; 6],
    mut outcome: Outcome,
) -> Outcome {
    let ready = match udp::deliver(&mut p, id, tag, w, &mut outcome) {
        Ok(true) => {
            // The server consumed its Call and the rejected datagrams. Requeue
            // the same pinned request, preserving its caller token and deadline.
            p.free_staged(id);
            let mut call = p.calls.remove(&id).expect("live UDP receive");
            call.taken = false;
            let next = p.next_id;
            p.next_id += 1;
            p.calls.insert(next, call);
            p.servers.iter().map(|&(t, _)| t).collect()
        }
        Ok(false) => p.settle(id, outcome).into_iter().collect(),
        Err((needed, available)) => {
            let token = p.close_call(id);
            drop(p);
            ready_all(token.into_iter().collect());
            return refused(needed, available);
        }
    };
    drop(p);
    ready_all(ready);
    Outcome::default()
}

/// True when the receive must wait again because ingress discarded its entire batch.
fn deliver(
    p: &mut Port,
    id: u64,
    tag: u64,
    w: [u64; 6],
    outcome: &mut Outcome,
) -> Result<bool, (u64, u64)> {
    let call = &p.calls[&id];
    // SAFETY: the reply was validated and the caller cannot collect until settlement.
    unsafe {
        let request = &*call.request;
        match tag {
            SOCKETS => {
                let needed = HANDLE_ROOM * w[5];
                let available = p.room - p.used;
                if needed > available {
                    return Err((needed, available));
                }
                p.used += needed;
                let mut keys = Vec::with_capacity(w[5] as usize);
                for offset in 0..w[5] {
                    let server_id = w[0] + offset;
                    p.handles.insert(server_id, Handle::new(Kind::Udp));
                    keys.push(p.issue_key(server_id));
                }
                served::deliver_address(request, address(w).unwrap(), keys);
            }
            R_ADDRESS => served::deliver_address(request, address(w).unwrap(), Vec::new()),
            PACKETS => {
                outcome.value =
                    served::deliver_receive(request, &call.staged_packets, &call.staged);
                return Ok(outcome.value == 0);
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(crate) fn release_udp(provider: &Provider, key: u64, retirement: Retirement) {
    let port = net_port(provider).unwrap();
    let mut p = lock(port);
    let id = p.keys.remove(&key).unwrap();
    if p.sealed {
        p.handles.remove(&id);
        p.used -= HANDLE_ROOM;
        drop(p);
        drop(retirement);
        return;
    }
    let stamp = p.next_id;
    p.next_id += 1;
    p.handles.get_mut(&id).unwrap().state = State::Released(stamp);
    p.udp_retired.insert(id, retirement);
    let servers = p.servers.iter().map(|&(t, _)| t).collect();
    drop(p);
    ready_all(servers);
}

/// # Safety
/// The server owns the typed port, call, and staged packet list.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_udp_fill_packets(
    context: *mut Context,
    served_key: u64,
    call_key: u64,
    packets: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: as above. The port lock pins the unsettled caller.
    unsafe {
        let (port, id) = call_of(context, call_key);
        let registry = &files(context).registry;
        let Cell::Port(own) = cell(registry.file_provider(served_key).state) else {
            unreachable!("a Served designates a port")
        };
        let mut p = lock(port);
        let call = &p.calls[&id];
        if !ptr::eq(&**own, port) || call.finished || call.answer.is_some() || call.words[0] != RECV
        {
            return Outcome::default();
        }
        let take = (call.words[3] as usize - call.staged_packets.len()).min((*packets).len);
        let needed = packet_charge(take);
        let available = p.room - p.used;
        if needed > available {
            return refused(needed, available);
        }
        p.used += needed;
        served::stage(
            &*packets,
            take,
            &mut p.calls.get_mut(&id).unwrap().staged_packets,
        );
        Outcome {
            value: take as u64,
            ..Outcome::default()
        }
    }
}
