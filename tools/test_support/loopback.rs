//! Loopback fixture peers. Keep connection identity independent of wire assertions.
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

/// Whether a process in `pid`'s tree holds the client end of the connection.
#[cfg(target_os = "linux")]
pub fn owns_client(pid: u32, client: SocketAddr, server: SocketAddr) -> io::Result<bool> {
    use std::fs;
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

/// Whether a process in `pid`'s tree holds the client end of the connection.
#[cfg(target_os = "macos")]
pub fn owns_client(pid: u32, client: SocketAddr, server: SocketAddr) -> io::Result<bool> {
    // sys/proc_info.h of the macOS 26 SDK: socket_fdinfo is 792 bytes, soi_kind
    // is at 256 and a TCP socket's in_sockinfo at 264, with the foreign port at
    // 0, the local port at 4, the foreign address at 32 and the local at 48.
    // An IPv4 address is the last four bytes of its sixteen.
    const SOCKET_FDINFO: usize = 792;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKINFO_TCP: i32 = 2;
    const KIND: usize = 256;
    const INFO: usize = 264;
    fn matches(info: &[u8], port: usize, at: usize, address: SocketAddr) -> bool {
        let held = u16::from_be_bytes([info[INFO + port], info[INFO + port + 1]]);
        let ip = &info[INFO + at..INFO + at + 16];
        held == address.port()
            && match address.ip() {
                IpAddr::V4(ip4) => ip[12..] == ip4.octets(),
                IpAddr::V6(ip6) => ip == ip6.octets(),
            }
    }
    let mut pending = vec![pid as libc::c_int];
    while let Some(pid) = pending.pop() {
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0
            };
            4096
        ];
        let bytes = (fds.len() * size_of::<libc::proc_fdinfo>()) as libc::c_int;
        // SAFETY: the buffer is live and writable for `bytes`.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                bytes,
            )
        };
        fds.truncate(got.max(0) as usize / size_of::<libc::proc_fdinfo>());
        for fd in fds {
            if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
                continue;
            }
            let mut info = [0u8; SOCKET_FDINFO];
            // SAFETY: info is live and writable for its length.
            let got = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd.proc_fd,
                    PROC_PIDFDSOCKETINFO,
                    info.as_mut_ptr().cast(),
                    SOCKET_FDINFO as libc::c_int,
                )
            };
            let kind = i32::from_ne_bytes(info[KIND..KIND + 4].try_into().unwrap());
            if got as usize == SOCKET_FDINFO
                && kind == SOCKINFO_TCP
                && matches(&info, 4, 48, client)
                && matches(&info, 0, 32, server)
            {
                return Ok(true);
            }
        }
        let mut children = vec![0 as libc::c_int; 4096];
        let bytes = (children.len() * size_of::<libc::c_int>()) as libc::c_int;
        // SAFETY: the buffer is live and writable for `bytes`.
        let got = unsafe { libc::proc_listchildpids(pid, children.as_mut_ptr().cast(), bytes) };
        children.truncate(got.max(0) as usize);
        pending.extend(children.into_iter().filter(|child| *child > 0));
    }
    Ok(false)
}
