//! Loopback fixture peers. Keep connection identity independent of wire assertions.
#[cfg(not(target_os = "macos"))]
use std::fs;
use std::{
    io,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
};

pub const STRAY_PREFIX: &str = "skipped unexpected loopback connection from ";

pub fn accept_from_address(listener: &TcpListener, expected: SocketAddr) -> io::Result<TcpStream> {
    loop {
        let (peer, address) = listener.accept()?;
        if address == expected {
            return Ok(peer);
        }
        log_stray(address);
    }
}

pub fn log_stray(address: SocketAddr) {
    eprintln!("{STRAY_PREFIX}{address}");
}

pub fn accept_from_process(listener: &TcpListener, pid: u32) -> io::Result<TcpStream> {
    loop {
        let (peer, address) = listener.accept()?;
        if owns_client(pid, address, peer.local_addr()?)? {
            return Ok(peer);
        }
        log_stray(address);
    }
}

pub fn accept_local(listener: &TcpListener) -> io::Result<TcpStream> {
    accept_from_process(listener, std::process::id())
}

/// macOS has no /proc: each process's own descriptor table names its TCP
/// sockets' endpoints (`proc_pidfdinfo`), and `proc_listchildpids` its children.
/// Loopback connects complete asynchronously there, so a connection not yet
/// visible in its owner's table is looked for again for up to 100 ms.
#[cfg(target_os = "macos")]
pub fn owns_client(pid: u32, client: SocketAddr, server: SocketAddr) -> io::Result<bool> {
    for _ in 0..20 {
        if scan_owner(pid, client, server) {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(false)
}

/// The rows of one `proc_pidinfo` listing, grown until the kernel's fit.
#[cfg(target_os = "macos")]
fn listing<T: Copy>(empty: T, fill: impl Fn(*mut T, i32) -> i32) -> Vec<T> {
    let mut rows = vec![empty; 256];
    loop {
        let bytes = (rows.len() * size_of::<T>()) as i32;
        let filled = usize::try_from(fill(rows.as_mut_ptr(), bytes)).unwrap_or(0);
        let count = filled / size_of::<T>();
        if count < rows.len() {
            rows.truncate(count);
            return rows;
        }
        rows = vec![empty; rows.len() * 4];
    }
}

#[cfg(target_os = "macos")]
fn scan_owner(pid: u32, client: SocketAddr, server: SocketAddr) -> bool {
    // struct socket_fdinfo: its size and the offsets of psi.soi_kind and the
    // TCP in_sockinfo's foreign and local ports, address family flag and
    // foreign and local addresses.
    const SIZE: usize = 792;
    const KIND: usize = 256;
    const FPORT: usize = 264;
    const LPORT: usize = 268;
    const VFLAG: usize = 288;
    const FADDR: usize = 296;
    const LADDR: usize = 312;
    const SOCKINFO_TCP: i32 = 2;
    const PROX_FDTYPE_SOCKET: u32 = 2;
    let word = |info: &[u8], at: usize| u32::from_le_bytes(info[at..at + 4].try_into().unwrap());
    let port = |info: &[u8], at: usize| u16::from_be(word(info, at) as u16);
    let address = |info: &[u8], at: usize| -> IpAddr {
        let bytes: [u8; 16] = info[at..at + 16].try_into().unwrap();
        if info[VFLAG] & 1 != 0 {
            IpAddr::from(<[u8; 4]>::try_from(&bytes[12..]).unwrap())
        } else {
            IpAddr::from(bytes)
        }
    };
    let mut pending = vec![pid as i32];
    while let Some(pid) = pending.pop() {
        let empty = libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        // SAFETY: each call fills at most `bytes` of the buffer it is given.
        let fds = listing(empty, |rows, bytes| unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, rows.cast(), bytes)
        });
        for fd in &fds {
            if fd.proc_fdtype != PROX_FDTYPE_SOCKET {
                continue;
            }
            let mut info = [0u8; SIZE];
            // SAFETY: the buffer holds SIZE bytes for the kernel to fill.
            let read = unsafe {
                libc::proc_pidfdinfo(pid, fd.proc_fd, 3, info.as_mut_ptr().cast(), SIZE as i32)
            };
            if read != SIZE as i32 || word(&info, KIND) as i32 != SOCKINFO_TCP {
                continue;
            }
            let local = SocketAddr::new(address(&info, LADDR), port(&info, LPORT));
            let foreign = SocketAddr::new(address(&info, FADDR), port(&info, FPORT));
            if local == client && foreign == server {
                return true;
            }
        }
        // SAFETY: each call fills at most `bytes` of the buffer it is given.
        // proc_listchildpids answers a count of PIDs, not bytes.
        pending.extend(listing(0i32, |rows, bytes| unsafe {
            libc::proc_listchildpids(pid, rows.cast(), bytes) * size_of::<i32>() as i32
        }));
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn owns_client(pid: u32, client: SocketAddr, server: SocketAddr) -> io::Result<bool> {
    fn endpoint(address: SocketAddr) -> String {
        let ip = match address.ip() {
            IpAddr::V4(ip) => format!("{:08X}", u32::from_ne_bytes(ip.octets())),
            IpAddr::V6(ip) => ip
                .octets()
                .chunks_exact(4)
                .map(|word| format!("{:08X}", u32::from_ne_bytes(word.try_into().unwrap())))
                .collect(),
        };
        format!("{ip}:{:04X}", address.port())
    }

    let client_end = endpoint(client);
    let server_end = endpoint(server);
    let table = fs::read_to_string(if client.is_ipv4() {
        "/proc/net/tcp"
    } else {
        "/proc/net/tcp6"
    })?;
    let inode = table.lines().skip(1).find_map(|line| {
        let mut row = line.split_ascii_whitespace().skip(1);
        if row.next()? == client_end && row.next()? == server_end {
            row.nth(6).filter(|inode| *inode != "0")
        } else {
            None
        }
    });
    let Some(inode) = inode else {
        return Ok(false);
    };
    let target = format!("socket:[{inode}]");
    let mut pending = vec![pid];
    while let Some(pid) = pending.pop() {
        if let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) {
            for fd in fds.filter_map(Result::ok) {
                if fs::read_link(fd.path()).is_ok_and(|path| path.as_os_str() == target.as_str()) {
                    return Ok(true);
                }
            }
        }
        if let Ok(threads) = fs::read_dir(format!("/proc/{pid}/task")) {
            for thread in threads.filter_map(Result::ok) {
                if let Ok(children) = fs::read_to_string(thread.path().join("children")) {
                    pending.extend(
                        children
                            .split_ascii_whitespace()
                            .filter_map(|id| id.parse::<u32>().ok()),
                    );
                }
            }
        }
    }
    Ok(false)
}
