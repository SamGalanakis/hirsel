//! The Linux address-change source: a `NETLINK_ROUTE` socket bound to the
//! IPv4 and IPv6 address groups, filled by one `RTM_GETADDR` dump and kept
//! current from `RTM_NEWADDR` and `RTM_DELADDR`. A reported `ENOBUFS` (lost
//! messages) dumps again.
use super::{Key, errno};
use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const NETLINK_ROUTE: i32 = 0;
/// `RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR`.
const GROUPS: u32 = 0x10 | 0x100;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const HEADER: usize = 16;
const IFADDRMSG: usize = 8;

/// The subscribed netlink socket.
pub(super) struct Changes {
    events: OwnedFd,
}

/// Subscribe to the address groups.
pub(super) fn subscribe() -> Result<Changes, i32> {
    let events = open()?;
    // SAFETY: an all-zero `sockaddr_nl` is valid; its fields are set below.
    let mut groups: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    groups.nl_family = libc::AF_NETLINK as u16;
    groups.nl_groups = GROUPS;
    // SAFETY: `groups` is a valid netlink address for this socket.
    let rc = unsafe {
        libc::bind(
            events.as_raw_fd(),
            (&groups as *const libc::sockaddr_nl).cast(),
            size_of::<libc::sockaddr_nl>() as u32,
        )
    };
    if rc < 0 {
        return Err(errno());
    }
    Ok(Changes { events })
}

impl Changes {
    /// Apply every queued change to `held`. Each change sets its key's state,
    /// so replaying changes older than the dump leaves the dump's answer. True
    /// when the kernel dropped messages and the table must be dumped again.
    pub(super) fn drain(&mut self, held: &mut BTreeSet<Key>) -> Result<bool, i32> {
        let mut lost = false;
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            match receive(&self.events, &mut buffer, libc::MSG_DONTWAIT) {
                Ok(len) => {
                    // A message that does not parse is a lost change.
                    for message in messages(&buffer[..len]) {
                        match message {
                            Ok((RTM_NEWADDR, Some(key))) => {
                                held.insert(key);
                            }
                            Ok((RTM_DELADDR, Some(key))) => {
                                held.remove(&key);
                            }
                            Ok((RTM_NEWADDR | RTM_DELADDR, None)) | Err(_) => lost = true,
                            Ok(_) => {}
                        }
                    }
                }
                Err(libc::EAGAIN) => return Ok(lost),
                Err(libc::ENOBUFS) => lost = true,
                Err(e) => return Err(e),
            }
        }
    }
}

fn open() -> Result<OwnedFd, i32> {
    // SAFETY: plain socket creation; the descriptor is owned at once.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(errno());
    }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn receive(fd: &OwnedFd, buffer: &mut [u8], flags: i32) -> Result<usize, i32> {
    loop {
        // SAFETY: `buffer` is writable for its length.
        let rc = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                flags,
            )
        };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        match errno() {
            libc::EINTR => {}
            e => return Err(e),
        }
    }
}

/// Every address assigned now, from one `RTM_GETADDR` dump on its own socket.
pub(super) fn snapshot() -> Result<BTreeSet<Key>, i32> {
    let fd = open()?;
    let mut request = [0u8; HEADER + IFADDRMSG];
    request[..4].copy_from_slice(&((HEADER + IFADDRMSG) as u32).to_ne_bytes());
    request[4..6].copy_from_slice(&RTM_GETADDR.to_ne_bytes());
    request[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    request[8..12].copy_from_slice(&1u32.to_ne_bytes());
    // ifa_family AF_UNSPEC: both families.
    loop {
        // SAFETY: `request` is readable for its length.
        let rc = unsafe { libc::send(fd.as_raw_fd(), request.as_ptr().cast(), request.len(), 0) };
        if rc >= 0 {
            break;
        }
        if errno() != libc::EINTR {
            return Err(errno());
        }
    }
    let mut held = BTreeSet::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let len = receive(&fd, &mut buffer, 0)?;
        for message in messages(&buffer[..len]) {
            match message? {
                (NLMSG_DONE, _) => return Ok(held),
                (RTM_NEWADDR, Some(key)) => {
                    held.insert(key);
                }
                _ => {}
            }
        }
    }
}

/// The netlink messages in one datagram, as (type, address key). An error
/// message is its errno; a malformed datagram is `EIO`.
fn messages(mut bytes: &[u8]) -> impl Iterator<Item = Result<(u16, Option<Key>), i32>> + '_ {
    std::iter::from_fn(move || {
        if bytes.len() < HEADER {
            return None;
        }
        let len = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
        if len < HEADER || len > bytes.len() {
            bytes = &[];
            return Some(Err(libc::EIO));
        }
        let kind = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        let body = &bytes[HEADER..len];
        bytes = &bytes[((len + 3) & !3).min(bytes.len())..];
        Some(match kind {
            NLMSG_ERROR => {
                let code = body
                    .get(..4)
                    .map_or(libc::EIO, |c| -i32::from_ne_bytes(c.try_into().unwrap()));
                if code == 0 {
                    Ok((kind, None))
                } else {
                    Err(code)
                }
            }
            // Only IPv4 and IPv6 groups are subscribed; another family is no address.
            RTM_NEWADDR | RTM_DELADDR
                if !matches!(
                    body.first().map(|&f| i32::from(f)),
                    Some(libc::AF_INET | libc::AF_INET6)
                ) =>
            {
                Ok((0, None))
            }
            RTM_NEWADDR | RTM_DELADDR => Ok((kind, address(body))),
            _ => Ok((kind, None)),
        })
    })
}

/// An `ifaddrmsg` and its attributes: the local address (`IFA_LOCAL`, which
/// on a point-to-point link differs from `IFA_ADDRESS`, the peer), else
/// `IFA_ADDRESS`.
fn address(body: &[u8]) -> Option<Key> {
    let family = match i32::from(*body.first()?) {
        libc::AF_INET => 0u8,
        libc::AF_INET6 => 1,
        _ => return None,
    };
    let prefix = *body.get(1)?;
    let index = u32::from_ne_bytes(body.get(4..8)?.try_into().unwrap());
    let mut attributes = body.get(IFADDRMSG..)?;
    let (mut local, mut any) = (None, None);
    while attributes.len() >= 4 {
        let len = usize::from(u16::from_ne_bytes(attributes[..2].try_into().unwrap()));
        let kind = u16::from_ne_bytes(attributes[2..4].try_into().unwrap());
        if len < 4 || len > attributes.len() {
            break;
        }
        let value = &attributes[4..len];
        let ip = match (family, value.len()) {
            (0, 4) => Some(u128::from(u32::from_be_bytes(value.try_into().unwrap()))),
            (1, 16) => Some(u128::from_be_bytes(value.try_into().unwrap())),
            _ => None,
        };
        match kind {
            IFA_LOCAL => local = ip,
            IFA_ADDRESS => any = ip,
            _ => {}
        }
        attributes = attributes.get((len + 3) & !3..).unwrap_or(&[]);
    }
    Some((family, local.or(any)?, index, prefix))
}
