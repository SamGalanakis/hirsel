use super::{
    HEADER_BYTES, Options, Socket, TurnBudget, UdpRules,
    socket::{checked, decode_address},
    socket_error,
};
use crate::host::udp as host;
use std::{
    collections::VecDeque,
    io,
    mem::MaybeUninit,
    net::SocketAddr,
    os::fd::{AsFd, AsRawFd},
};

const NAME_BYTES: usize = size_of::<libc::sockaddr_storage>();
const CONTROL_BYTES: usize = HEADER_BYTES - NAME_BYTES;
const MAX_MESSAGES: usize = 64;

struct Record {
    slot: usize,
    peer: Option<SocketAddr>,
    ecn: u8,
    length: usize,
    captured: usize,
    stride: Option<usize>,
    cursor: usize,
    captured_at: u64,
}

impl Record {
    fn len(&self) -> usize {
        self.stride
            .unwrap_or(self.length)
            .min(self.length - self.cursor)
    }
}

pub enum Captured<'a> {
    Datagram {
        peer: SocketAddr,
        ecn: u8,
        length: usize,
        payload: &'a [u8],
        captured_at: u64,
    },
    Discarded,
}

pub struct ReceivePool {
    options: Options,
    slots: Box<[MaybeUninit<u64>]>,
    free: Vec<usize>,
    queue: VecDeque<Record>,
}

impl ReceivePool {
    pub fn new(options: Options) -> io::Result<Self> {
        let words = options.slots() * (options.slot_bytes() / 8);
        let exhausted = |_| socket_error(libc::ENOMEM);
        let slots = super::resident(words, MaybeUninit::uninit)?;
        let mut free = Vec::new();
        free.try_reserve_exact(options.slots()).map_err(exhausted)?;
        free.extend((0..options.slots()).rev());
        let mut queue = VecDeque::new();
        queue
            .try_reserve_exact(options.slots())
            .map_err(exhausted)?;
        Ok(Self {
            options,
            slots,
            free,
            queue,
        })
    }

    /// Captures from [`Self::read`] calls until `want` are taken or a second
    /// passes: Darwin's loopback hands a datagram to its input thread, so it
    /// can land after the send returns. Stops at the first read that takes
    /// `want` or more, so a single read's count is still checked.
    #[cfg(test)]
    pub(crate) fn read_arrived(
        &mut self,
        socket: &Socket,
        budget: &mut TurnBudget,
        want: usize,
    ) -> io::Result<usize> {
        let mut taken = 0;
        for _ in 0..1000 {
            match self.read(socket, None, budget) {
                Ok(count) => taken += count,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if taken >= want {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(taken)
    }

    /// Capture into currently free slots. A zero read limit leaves the descriptor untouched.
    pub fn read(
        &mut self,
        socket: &Socket,
        ingress: Option<&UdpRules>,
        budget: &mut TurnBudget,
    ) -> io::Result<usize> {
        let count = budget.read_limit(self.free.len(), self.options);
        if count == 0 {
            return Ok(0);
        }
        let base = self.slots.as_mut_ptr().cast::<u8>();
        let room = self.options.payload_room();
        let stride = self.options.slot_bytes();
        // SAFETY: zero is valid for all integer and pointer fields in these C headers.
        let mut messages: [host::Message; MAX_MESSAGES] = unsafe { std::mem::zeroed() };
        let mut iovecs: [libc::iovec; MAX_MESSAGES] = unsafe { std::mem::zeroed() };
        let free = self.free.len();
        for i in 0..count {
            let slot = self.free[free - 1 - i];
            // SAFETY: each distinct free slot names an allocated stride, including the header.
            let at = unsafe { base.add(slot * stride) };
            // SAFETY: initialize sockaddr and ancillary padding the kernel may leave unwritten.
            unsafe { at.write_bytes(0, HEADER_BYTES) };
            let header = &mut messages[i].msg_hdr;
            header.msg_name = at.cast();
            header.msg_namelen = NAME_BYTES as _;
            // SAFETY: both offsets are within this initialized header and slot.
            header.msg_control = unsafe { at.add(NAME_BYTES) }.cast();
            header.msg_controllen = CONTROL_BYTES as _;
            iovecs[i] = libc::iovec {
                iov_base: unsafe { at.add(HEADER_BYTES) }.cast(),
                iov_len: room,
            };
            header.msg_iov = &mut iovecs[i];
            header.msg_iovlen = 1;
        }
        // SAFETY: all headers and iovecs stay pinned through the call. Each writable slot
        // in the batch is distinct and has the declared header and payload capacity.
        let received =
            checked(unsafe { host::receive(socket.as_fd().as_raw_fd(), &mut messages[..count]) })?
                as usize;
        let captured_at = super::measure::clock();
        for (i, message) in messages[..received].iter().enumerate() {
            let slot = self.free[free - 1 - i];
            let length = message.msg_len as usize;
            let captured = length.min(room);
            budget.captured(captured);
            let header = &message.msg_hdr;
            // SAFETY: each header's name and control space were initialized before issue.
            let peer = unsafe { decode_address(header.msg_name.cast(), header.msg_namelen) };
            let control_len = header.msg_controllen as usize;
            let controls = (control_len <= CONTROL_BYTES).then(|| {
                // SAFETY: control space was initialized and the reported length fits it.
                unsafe { std::slice::from_raw_parts(header.msg_control.cast::<u8>(), control_len) }
            });
            let metadata = controls.and_then(|control| metadata(control, header.msg_flags));
            let (stride, ecn) = metadata.unwrap_or((None, 0));
            let peer = peer.filter(|&peer| {
                metadata.is_some()
                    && ingress.is_none_or(|rules| {
                        let whole = stride.map_or(1, |stride| {
                            if captured == length {
                                length.div_ceil(stride)
                            } else {
                                captured / stride
                            }
                        });
                        rules.permits_ingress(peer, whole as u64)
                    })
            });
            self.queue.push_back(Record {
                slot,
                peer,
                ecn,
                length,
                captured,
                stride,
                cursor: 0,
                captured_at,
            });
        }
        self.free.truncate(free - received);
        Ok(received)
    }

    pub fn front(&self) -> Option<Captured<'_>> {
        let record = self.queue.front()?;
        let length = record.len();
        let Some(peer) = record.peer else {
            return Some(Captured::Discarded);
        };
        let end = record.cursor + length;
        if record.stride.is_some() && end > record.captured {
            return Some(Captured::Discarded);
        }
        let start = record.slot * self.options.slot_bytes() + HEADER_BYTES + record.cursor;
        let size = end.min(record.captured) - record.cursor;
        // SAFETY: the checked payload range is inside the bytes initialized by recvmmsg.
        let payload = unsafe {
            std::slice::from_raw_parts(self.slots.as_ptr().cast::<u8>().add(start), size)
        };
        Some(Captured::Datagram {
            peer,
            ecn: record.ecn,
            length,
            payload,
            captured_at: record.captured_at,
        })
    }

    pub fn advance(&mut self) {
        if let Some(record) = self.queue.front_mut() {
            record.cursor += record.len();
            if record.cursor == record.length {
                let record = self.queue.pop_front().unwrap();
                self.free.push(record.slot);
            }
        }
    }
}

fn metadata(mut control: &[u8], flags: i32) -> Option<(Option<usize>, u8)> {
    if flags & libc::MSG_CTRUNC != 0 {
        return None;
    }
    let mut stride = None;
    let mut ecn = 0;
    let header_size = size_of::<libc::cmsghdr>().next_multiple_of(host::CMSG_ALIGN);
    while control.len() >= header_size {
        // SAFETY: the slice contains a complete C header and arbitrary integer bits are valid.
        let header = unsafe { control.as_ptr().cast::<libc::cmsghdr>().read_unaligned() };
        let size = header.cmsg_len as usize;
        let value = control.get(header_size..size)?;
        match (header.cmsg_level, header.cmsg_type) {
            kind if Some(kind) == host::COALESCED => {
                let value = i32::from_ne_bytes(value.try_into().ok()?);
                if value <= 0 || stride.is_some() {
                    return None;
                }
                stride = Some(value as usize);
            }
            (libc::IPPROTO_IP, host::TOS_RECEIVED) => ecn = *value.first()? & 3,
            (libc::IPPROTO_IPV6, libc::IPV6_TCLASS) => {
                ecn = (i32::from_ne_bytes(value.try_into().ok()?) & 3) as u8;
            }
            _ => {}
        }
        let following = size.next_multiple_of(host::CMSG_ALIGN);
        control = control.get(following..).unwrap_or(&[]);
    }
    Some((stride, ecn))
}
