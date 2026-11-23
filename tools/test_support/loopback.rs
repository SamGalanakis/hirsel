//! Loopback fixture peers. Keep connection identity independent of wire assertions.
use std::{
    fs, io,
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
