//! A recorded TCP socket or listener that crosses units (docs/recording.md,
//! "Sockets that cross"). Every recording unit names its taped sockets by
//! its own object ordinals. The unit operation that carries one writes its
//! snapshot (tag 2 or 3); the sender's object loses its source there, and the
//! receiving unit's take gives it a new ordinal whose source is the socket's
//! reactor key in the receiving unit. A socket that comes back without a take
//! (a refused send) keeps the sender's ordinal.
use super::super::*;
use crate::net::CrossingSockets;

/// A TCP socket's (tag 2) or listener's (tag 3) snapshot, as its own stream
/// proves it.
#[derive(Clone, Copy)]
pub(crate) enum Snapshot {
    /// A socket: whether an ACCEPT delivered it (else a CONNECT made it),
    /// the peer address that operation named, and the state its own
    /// receives showed. Bit 0: a receive with room delivered no bytes.
    /// Bit 1: an operation on it failed with a reset.
    Stream {
        accepted: bool,
        address: [u8; 19],
        state: u8,
    },
    /// A listener: the address its LISTEN bound.
    Listener { address: [u8; 19] },
}

impl Snapshot {
    /// A handle part's snapshot after its part tag: kind, ordinal, body.
    fn part(&self, ordinal: u64) -> Vec<u8> {
        let mut out = vec![];
        match *self {
            Self::Stream {
                accepted,
                address,
                state,
            } => {
                out.push(2);
                put_varint(&mut out, ordinal);
                out.push(u8::from(accepted));
                out.extend_from_slice(&address);
                // No TLS: a socket with a TLS session never crosses.
                out.push(0);
                out.push(state);
            }
            Self::Listener { address } => {
                out.push(3);
                put_varint(&mut out, ordinal);
                // A listener's direction byte is always 0.
                out.push(0);
                out.extend_from_slice(&address);
            }
        }
        out
    }
}

/// Record a delivery's effect on a socket's snapshot: a new socket or
/// listener, or the state a receive or a reset shows.
pub(in crate::tape) fn delivered(
    live: &mut Live,
    r: &Request,
    before: usize,
    created: bool,
    error: u8,
) {
    let op = r.operation;
    if created && matches!(op, CONNECT | ACCEPT | LISTEN) {
        // SAFETY: an address operation pins its network operand.
        let network = unsafe { &*r.path.cast::<crate::net::NetworkRequest>() };
        let address = address_bytes(network.address);
        live.sockets.insert(
            live.objects.len(),
            if op == LISTEN {
                Snapshot::Listener { address }
            } else {
                Snapshot::Stream {
                    accepted: op == ACCEPT,
                    address,
                    state: 0,
                }
            },
        );
        return;
    }
    let Some(Snapshot::Stream { state, .. }) = live.sockets.get_mut(&ordinal_of(r.handle)) else {
        return;
    };
    if error == 0 && op == RECV_BOUNDED && r.buffer.len == before && r.buffer.capacity > before {
        *state |= 1;
    }
    if error == 5 {
        *state |= 2;
    }
}

/// A taped socket in transit: its key and object in the sending unit, its
/// reactor source's provider (absent when replaying), its snapshot, and the
/// object its take gave it.
pub(crate) struct Departed {
    key: u64,
    cell: usize,
    generation: u32,
    ordinal: usize,
    source: Option<Provider>,
    snapshot: Option<Snapshot>,
    arrival: Option<(usize, u32, usize)>,
}

// SAFETY: the source provider is run-pinned and moves as opaque data.
unsafe impl Send for Departed {}

impl Departed {
    pub(crate) fn source(&self) -> Option<Provider> {
        self.source
    }
}

/// The socket carrier `key` (tape `handle`) leaves its unit: its object no
/// longer reaches a source. None when the tape does not hold it.
pub(crate) fn depart(provider: &Provider, key: u64, handle: u64) -> Option<Box<Departed>> {
    let cell = cell_of(provider.state);
    let generation = generation_of(handle);
    let mut state = cell.state.lock().unwrap();
    if state.generation != generation {
        return None;
    }
    let live = state.live.as_mut()?;
    let ordinal = ordinal_of(handle);
    let inner = live.objects.get_mut(ordinal.checked_sub(1)?)?.inner.take();
    if inner.is_none() && live.mode != Mode::Replay {
        return None;
    }
    Some(Box::new(Departed {
        key,
        cell: provider.state as usize,
        generation,
        ordinal,
        source: inner.map(|i| i.provider),
        snapshot: live.sockets.get(&ordinal).copied(),
        arrival: None,
    }))
}

/// Parse a `Socket` (tag 2) or `Listener` (tag 3) snapshot body after its ordinal.
pub(in crate::tape) fn parse_snapshot(
    tag: u8,
    c: &mut format::Cursor<'_>,
) -> Result<Snapshot, super::replay::Unpacked> {
    use super::replay::Unpacked;
    match tag {
        2 => {
            let accepted = match c.u8()? {
                0 => false,
                1 => true,
                _ => return Err(Unpacked::Layout),
            };
            let address: [u8; 19] = c.fixed()?;
            // A socket with a TLS session never crosses.
            if c.u8()? != 0 {
                return Err(Unpacked::Layout);
            }
            let state = c.u8()?;
            if state & !3 != 0 {
                return Err(Unpacked::Layout);
            }
            Ok(Snapshot::Stream {
                accepted,
                address,
                state,
            })
        }
        3 => {
            if c.u8()? != 0 {
                return Err(Unpacked::Layout);
            }
            let address: [u8; 19] = c.fixed()?;
            Ok(Snapshot::Listener { address })
        }
        _ => Err(Unpacked::Layout),
    }
}

/// The `TcpOrigin` a replayed `Snapshot` names for `files.tcp_routes`.
pub(in crate::tape) fn snapshot_origin(snapshot: Snapshot) -> crate::net::TcpOrigin {
    match snapshot {
        Snapshot::Stream { accepted: true, .. } => crate::net::TcpOrigin::Accepted,
        Snapshot::Stream {
            accepted: false,
            address,
            ..
        } => {
            let mut c = format::Cursor::new(&address);
            let addr = crate::net::Address {
                family: c.u8().unwrap_or(0),
                hi: u64::from_le_bytes(c.fixed().unwrap_or([0; 8])),
                lo: u64::from_le_bytes(c.fixed().unwrap_or([0; 8])),
                port: u16::from_le_bytes(c.fixed().unwrap_or([0; 2])),
            };
            crate::net::TcpOrigin::Connected(addr)
        }
        Snapshot::Listener { .. } => crate::net::TcpOrigin::Listener,
    }
}

/// A crossed socket attached as reactor key `reactor` in a unit whose taped
/// sockets are on `provider` (generation of `handle`): its carrier there.
/// The object its take named, or its own on the way back to its sender, or
/// else a new one, which cuts a recording stream that never named it.
pub(crate) fn settle(
    provider: &Provider,
    handle: u64,
    departed: Option<&Departed>,
    source: Provider,
    reactor: u64,
) -> Option<u64> {
    let cell = cell_of(provider.state);
    let generation = generation_of(handle);
    let at = provider.state as usize;
    let mut state = cell.state.lock().unwrap();
    if state.generation != generation {
        return None;
    }
    let live = state.live.as_mut()?;
    let named = departed.and_then(|d| match d.arrival {
        Some((c, g, o)) if c == at && g == generation => Some(o),
        _ if d.cell == at && d.generation == generation => Some(d.ordinal),
        _ => None,
    });
    let ordinal = match named.filter(|&o| o >= 1 && o <= live.objects.len()) {
        Some(o) => o,
        None => {
            if live.recording() {
                live.cut(UNNAMED_SOCKET);
            }
            live.unit_object() as usize
        }
    };
    live.objects[ordinal - 1].inner = Some(Inner {
        provider: source,
        handle: reactor,
    });
    Some((ordinal as u64) << 2)
}

const UNNAMED_SOCKET: &str = "a socket arrived that the recording did not name";
const UNWRITTEN_SOCKET: &str = "a value holds a socket whose snapshot the recording did not write";

/// A socket's part as its sender writes it, with `ordinal`: the sender's
/// own in an item, 0 in a helper's `END`.
pub(in crate::tape) fn sent(
    sockets: Option<&CrossingSockets>,
    word: u64,
    ordinal: Option<u64>,
) -> Result<Vec<u8>, &'static str> {
    let crossing = sockets.ok_or(UNWRITTEN_SOCKET)?;
    let mut crossing = crossing.lock().unwrap();
    let d = crossing.departed(word).ok_or(UNWRITTEN_SOCKET)?;
    let snapshot = d.snapshot.ok_or(UNWRITTEN_SOCKET)?;
    Ok(snapshot.part(ordinal.unwrap_or(d.ordinal as u64)))
}

/// A socket arriving in `live` (cell `at`, `generation`): a new ordinal,
/// which its attach in this unit names.
pub(in crate::tape) fn arrived(
    live: &mut Live,
    at: usize,
    generation: u32,
    sockets: Option<&CrossingSockets>,
    word: u64,
) -> Result<Vec<u8>, &'static str> {
    let crossing = sockets.ok_or(UNWRITTEN_SOCKET)?;
    let mut crossing = crossing.lock().unwrap();
    let d = crossing.departed(word).ok_or(UNWRITTEN_SOCKET)?;
    let snapshot = d.snapshot.ok_or(UNWRITTEN_SOCKET)?;
    let ordinal = live.unit_object() as usize;
    live.sockets.insert(ordinal, snapshot);
    d.arrival = Some((at, generation, ordinal));
    Ok(snapshot.part(ordinal as u64))
}

impl Departed {
    pub(crate) fn key(&self) -> u64 {
        self.key
    }
}
