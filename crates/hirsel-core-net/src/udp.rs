//! Provider-independent UDP validation and the Linux datagram transport.

use std::{io, net::SocketAddr, num::NonZeroU16};

mod rules;
pub use rules::UdpRules;
mod linux;
pub(crate) mod measure;
mod members;
mod receive;
mod send;
pub use linux::Socket;
pub use members::{MemberProducer, MemberReader, WriterList, member_queue};
pub use receive::{Captured, ReceivePool};
pub use send::Window;

pub const HEADER_BYTES: usize = 256;
pub const FULL_SLOT_BYTES: u32 = 65_792;
const ENTRY_STEP: u128 = 64;
const BYTE_STEP: u128 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    slots: u32,
    slot_bytes: u32,
}

impl Options {
    pub fn new(slots: u32, slot_bytes: u32) -> Option<Self> {
        (slots.is_power_of_two()
            && slots <= 32_768
            && (1_536..=FULL_SLOT_BYTES).contains(&slot_bytes)
            && slot_bytes % 8 == 0)
            .then_some(Self { slots, slot_bytes })
    }

    pub fn slots(self) -> usize {
        self.slots as usize
    }

    pub fn slot_bytes(self) -> usize {
        self.slot_bytes as usize
    }

    pub fn payload_room(self) -> usize {
        self.slot_bytes() - HEADER_BYTES
    }

    pub fn charge(self, members: Option<NonZeroU16>) -> u64 {
        // NET-16 bounds all three operands: the maximum needs fewer than 48 bits.
        let pools = 1 + members.map_or(0, |n| u64::from(n.get()));
        pools * u64::from(self.slots) * u64::from(self.slot_bytes)
    }

    pub fn receive_steps(self, room: u64, entries: u64) -> (u64, u64) {
        let entries = u128::from(entries);
        let bytes = u128::from(room).min(entries * self.payload_room() as u128);
        (
            entries.div_ceil(ENTRY_STEP) as u64,
            bytes.div_ceil(BYTE_STEP) as u64,
        )
    }
}

pub fn send_entry_steps(packets: u64) -> u64 {
    u128::from(packets).div_ceil(ENTRY_STEP) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxPacket {
    pub to: SocketAddr,
    pub at: u64,
    pub len: u64,
    pub ecn: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendRefusal {
    Denied,
    Oversize(u32),
}

pub const fn max_payload(v6: bool) -> u32 {
    if v6 { 65_527 } else { 65_507 }
}

pub fn send_byte_steps(
    packets: impl Iterator<Item = TxPacket>,
    data_len: u64,
    v6: bool,
    rules: &UdpRules,
) -> Result<u128, SendRefusal> {
    let mut sum = 0u128;
    let limit = max_payload(v6);
    for packet in packets {
        if packet.to.is_ipv6() != v6 || packet.ecn > 3 || !rules.permits_send(packet.to) {
            return Err(SendRefusal::Denied);
        }
        if packet.len > u64::from(limit) {
            return Err(SendRefusal::Oversize(limit));
        }
        if u128::from(packet.at) + u128::from(packet.len) > u128::from(data_len) {
            return Err(SendRefusal::Denied);
        }
        sum += u128::from(packet.len);
    }
    Ok(sum.div_ceil(BYTE_STEP))
}

/// The record a provider stages before the caller computes its offset and truncation flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub peer: SocketAddr,
    pub len: u64,
    pub appended: u64,
    pub ecn: u8,
}

pub fn valid_receive(
    v6: bool,
    ingress: Option<&UdpRules>,
    payload_room: usize,
    byte_room: u64,
    entry_room: u64,
    received: &[Packet],
    bytes: usize,
) -> bool {
    if received.is_empty() || received.len() as u64 > entry_room {
        return false;
    }
    let mut room = byte_room;
    for (index, packet) in received.iter().enumerate() {
        let fit = room.min(payload_room as u64).min(packet.len);
        if packet.peer.is_ipv6() != v6
            || packet.len > u64::from(max_payload(v6))
            || packet.ecn > 3
            || ingress.is_some_and(|rules| !rules.permits_send(packet.peer))
            || packet.appended != fit
            || index != 0 && packet.len != fit
        {
            return false;
        }
        room -= fit;
    }
    byte_room - room == bytes as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steer {
    Hash,
    Quic,
}

impl Steer {
    pub fn select(self, members: NonZeroU16, peer: SocketAddr, payload: &[u8]) -> u16 {
        fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
            bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            })
        }

        let members = members.get();
        let reduce = |hash| (hash % u64::from(members)) as u16;
        let member = |bytes: &[u8]| {
            let field = bytes.get(1..3)?;
            let index = u16::from_be_bytes([field[0], field[1]]);
            (index < members).then_some(index)
        };
        if self == Self::Quic {
            if payload.first().is_some_and(|byte| byte & 0x80 != 0) {
                if let Some(&d) = payload.get(5)
                    && d != 0
                    && let Some(id) = payload.get(6..6 + usize::from(d))
                {
                    return member(id).unwrap_or_else(|| reduce(fnv1a64(id.iter().copied())));
                }
            } else if let Some(index) = payload.get(1..).and_then(member) {
                return index;
            }
        }
        let port = peer.port().to_be_bytes();
        reduce(match peer {
            SocketAddr::V4(addr) => fnv1a64([4].into_iter().chain(addr.ip().octets()).chain(port)),
            SocketAddr::V6(addr) => fnv1a64([6].into_iter().chain(addr.ip().octets()).chain(port)),
        })
    }
}

/// Work remaining for all UDP sources on one poller turn.
pub struct TurnBudget {
    datagrams: usize,
    bytes: usize,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            datagrams: 64,
            bytes: 256 * 1024,
        }
    }
}

impl TurnBudget {
    pub(crate) fn paused() -> Self {
        Self {
            datagrams: 0,
            bytes: 0,
        }
    }

    pub fn read_limit(&self, free: usize, options: Options) -> usize {
        free.min(self.datagrams)
            .min(self.bytes / options.payload_room())
    }

    pub fn captured(&mut self, bytes: usize) {
        self.bytes -= bytes;
    }

    pub fn dispatch(&mut self, copied_bytes: usize) -> bool {
        if self.datagrams == 0 || copied_bytes > self.bytes {
            return false;
        }
        self.datagrams -= 1;
        self.bytes -= copied_bytes;
        true
    }

    pub(crate) fn has_datagrams(&self) -> bool {
        self.datagrams != 0
    }
}

/// Receive storage of `words` elements, every page resident before the socket's first read.
/// A first receive into untouched storage takes one fault per page inside the kernel copy;
/// at 1M datagrams/s that held the reader on its CPU for ~250 µs, past what the socket's
/// default queue holds, and dropped a GSO train. The volatile writes keep the allocation
/// from staying a lazily mapped zero region.
fn resident<T>(words: usize, fill: impl FnMut() -> T) -> io::Result<Box<[T]>> {
    const PAGE: usize = 4096;
    let mut storage = Vec::new();
    storage
        .try_reserve_exact(words)
        .map_err(|_| socket_error(libc::ENOMEM))?;
    storage.resize_with(words, fill);
    let mut storage = storage.into_boxed_slice();
    let base = storage.as_mut_ptr().cast::<u8>();
    for offset in (0..words * size_of::<T>()).step_by(PAGE) {
        // SAFETY: the offset is inside this exclusively owned allocation, and every
        // element type stored here accepts any byte value.
        unsafe { base.add(offset).write_volatile(0) };
    }
    Ok(storage)
}

fn socket_error(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}
