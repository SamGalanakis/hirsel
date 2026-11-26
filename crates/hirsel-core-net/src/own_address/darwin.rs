//! The macOS address-change source: `getifaddrs` for the snapshot and a
//! `PF_ROUTE` socket for the changes. A route message is a signal, not a delta:
//! any `RTM_NEWADDR` or `RTM_DELADDR`, a reported `ENOBUFS` (lost messages) or a
//! message that does not parse takes a new snapshot, so the table is never
//! built from a parsed `ifa_msghdr`.
pub(super) use super::ifaddrs::snapshot;
use super::{Key, errno};
use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The subscribed routing socket.
pub(super) struct Changes {
    events: OwnedFd,
}

/// Open the routing socket, which receives every routing message the kernel
/// sends, address changes among them.
pub(super) fn subscribe() -> Result<Changes, i32> {
    // SAFETY: plain socket creation; the descriptor is owned at once.
    let fd = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
    if fd < 0 {
        return Err(errno());
    }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    let events = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: `events` is an open descriptor.
    if unsafe { libc::fcntl(events.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(errno());
    }
    Ok(Changes { events })
}

impl Changes {
    /// Read every queued routing message. True when one changed an address or
    /// was lost, and the table must be read again.
    pub(super) fn drain(&mut self, _held: &mut BTreeSet<Key>) -> Result<bool, i32> {
        let mut changed = false;
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            // SAFETY: `buffer` is writable for its length.
            let rc = unsafe {
                libc::recv(
                    self.events.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if rc >= 0 {
                // A routing socket delivers one message per read: `rtm_msglen`,
                // `rtm_version`, `rtm_type` lead every header.
                let message = &buffer[..rc as usize];
                changed |= match message {
                    [_, _, version, kind, ..] if i32::from(*version) == libc::RTM_VERSION => {
                        matches!(i32::from(*kind), libc::RTM_NEWADDR | libc::RTM_DELADDR)
                    }
                    _ => true,
                };
                continue;
            }
            match errno() {
                libc::EINTR => {}
                libc::EAGAIN => return Ok(changed),
                libc::ENOBUFS => changed = true,
                e => return Err(e),
            }
        }
    }
}
