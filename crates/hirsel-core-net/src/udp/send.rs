use super::{
    Socket, TxPacket,
    linux::{address, checked},
    socket_error,
};
use std::{
    io,
    os::fd::{AsFd, AsRawFd},
    sync::atomic::Ordering,
};

const MESSAGES: usize = 32;
const VECTORS: usize = 64;
const MAX_TRAIN_BYTES: u64 = 65_507;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Sent(usize),
    RetryAsSingles,
}

#[repr(align(8))]
struct Control([u8; 64]);

impl Control {
    fn put(&mut self, offset: usize, level: i32, kind: i32, data: &[u8]) -> usize {
        let word = super::CMSG_ALIGN;
        let header = size_of::<libc::cmsghdr>().next_multiple_of(word);
        let length = header + data.len();
        let padded = length.next_multiple_of(word);
        assert!(offset + padded <= self.0.len());
        let cmsg = libc::cmsghdr {
            cmsg_len: length as _,
            cmsg_level: level,
            cmsg_type: kind,
        };
        // SAFETY: the checked control range holds a complete cmsg header.
        unsafe {
            self.0
                .as_mut_ptr()
                .add(offset)
                .cast::<libc::cmsghdr>()
                .write_unaligned(cmsg)
        };
        self.0[offset + header..offset + length].copy_from_slice(data);
        offset + padded
    }
}

impl Socket {
    /// Attempt one nonblocking sendmmsg window. The result counts logical
    /// datagrams, including every packet in each accepted GSO message.
    pub fn send_window(&self, data: &[u8], packets: &[TxPacket]) -> io::Result<Window> {
        if packets.is_empty() {
            return Ok(Window::Sent(0));
        }
        let gso = self.gso.load(Ordering::Relaxed);
        // SAFETY: zero is valid for the C structures' integer and pointer fields.
        let mut messages: [crate::sys::mmsghdr; MESSAGES] = unsafe { std::mem::zeroed() };
        let mut sockets: [libc::sockaddr_storage; MESSAGES] = unsafe { std::mem::zeroed() };
        let mut vectors: [libc::iovec; VECTORS] = unsafe { std::mem::zeroed() };
        let mut controls = std::array::from_fn::<_, MESSAGES, _>(|_| Control([0; 64]));
        let mut ends = [0; MESSAGES];
        let mut at = 0;
        let mut messages_used = 0;
        while messages_used < MESSAGES && at < packets.len() {
            let train = if gso { train_len(&packets[at..]) } else { 1 };
            if at + train > VECTORS {
                break;
            }
            let first = packets[at];
            let (raw, addr_len) = address(first.to);
            sockets[messages_used] = raw;
            for (vector, packet) in vectors[at..at + train]
                .iter_mut()
                .zip(&packets[at..at + train])
            {
                let start = usize::try_from(packet.at).map_err(|_| socket_error(libc::EINVAL))?;
                let end = packet
                    .at
                    .checked_add(packet.len)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| socket_error(libc::EINVAL))?;
                let payload = data
                    .get(start..end)
                    .ok_or_else(|| socket_error(libc::EINVAL))?;
                *vector = libc::iovec {
                    iov_base: payload.as_ptr().cast_mut().cast(),
                    iov_len: payload.len(),
                };
            }
            let control = &mut controls[messages_used];
            let ecn = i32::from(first.ecn).to_ne_bytes();
            #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
            let mut control_len = if self.v6 {
                control.put(0, libc::IPPROTO_IPV6, libc::IPV6_TCLASS, &ecn)
            } else {
                control.put(0, libc::IPPROTO_IP, libc::IP_TOS, &ecn)
            };
            #[cfg(target_os = "linux")]
            if train > 1 {
                control_len = control.put(
                    control_len,
                    libc::SOL_UDP,
                    libc::UDP_SEGMENT,
                    &(first.len as u16).to_ne_bytes(),
                );
            }
            let header = &mut messages[messages_used].msg_hdr;
            header.msg_name = (&mut sockets[messages_used] as *mut libc::sockaddr_storage).cast();
            header.msg_namelen = addr_len;
            header.msg_iov = &mut vectors[at];
            header.msg_iovlen = train as _;
            header.msg_control = control.0.as_mut_ptr().cast();
            header.msg_controllen = control_len as _;
            at += train;
            ends[messages_used] = at;
            messages_used += 1;
        }
        // SAFETY: the address, ancillary data, iovecs and borrowed payload remain live
        // through this syscall. The kernel writes only the supplied message headers.
        let sent = checked(unsafe {
            crate::sys::sendmmsg(
                self.as_fd().as_raw_fd(),
                messages.as_mut_ptr(),
                messages_used as u32,
                libc::MSG_DONTWAIT | crate::sys::MSG_NOSIGNAL,
            )
        });
        match sent {
            Ok(0) => Ok(Window::Sent(0)),
            Ok(n) => Ok(Window::Sent(ends[n as usize - 1])),
            Err(error)
                if ends[0] > 1
                    && matches!(error.raw_os_error(), Some(libc::EIO | libc::EINVAL)) =>
            {
                self.gso.store(false, Ordering::Relaxed);
                Ok(Window::RetryAsSingles)
            }
            Err(error) => Err(error),
        }
    }
}

fn train_len(packets: &[TxPacket]) -> usize {
    let first = packets[0];
    if first.len == 0 || first.len > MAX_TRAIN_BYTES {
        return 1;
    }
    let mut bytes = first.len;
    let mut count = 1;
    for next in packets[1..].iter().take(VECTORS - 1) {
        if next.to != first.to
            || next.ecn != first.ecn
            || next.len == 0
            || next.len > first.len
            || bytes + next.len > MAX_TRAIN_BYTES
        {
            break;
        }
        bytes += next.len;
        count += 1;
        if next.len < first.len {
            break;
        }
    }
    count
}
