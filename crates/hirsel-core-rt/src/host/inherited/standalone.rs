use crate::{
    entry::prepare_hex,
    net::{Address, NetworkRequest},
    os_net::{error, register_socket, retry},
    provider::{Outcome, Request, Step},
};
use std::{
    env, io,
    net::{SocketAddr, TcpListener},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) const LISTEN_FD: i32 = 3;
static LISTENER_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Take custody of the single socket passed to this process by its launcher.
/// # Safety
/// Matching activation metadata must designate a descriptor this process
/// inherited at exec and has not otherwise taken into ownership. Receive
/// before any other thread can access the process environment.
pub(crate) unsafe fn receive_listener() -> io::Result<Option<OwnedFd>> {
    let Some(pid) = env::var_os("LISTEN_PID") else {
        return Ok(None);
    };
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    let pid: u32 = pid
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)?;
    if pid != crate::host::sys::activation_pid() {
        return Ok(None);
    }
    let count = env::var_os("LISTEN_FDS")
        .and_then(|n| n.to_str()?.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    if count != 1 {
        return Err(invalid());
    }
    if LISTENER_RECEIVED.swap(true, Ordering::AcqRel) {
        return Ok(None);
    }
    // SAFETY: the launcher assigned this fd to this PID. Verify it exists
    // before construction, and stop it from passing through any child exec.
    unsafe {
        retry(|| libc::fcntl(LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) as isize)
            .map_err(io::Error::from_raw_os_error)?;
        let listener = OwnedFd::from_raw_fd(LISTEN_FD);
        // SAFETY: the launch receives the socket before any other thread can
        // access the environment. Leave no activation metadata for children.
        for name in [
            "LISTEN_PID",
            "LISTEN_PIDFDID",
            "LISTEN_FDS",
            "LISTEN_FDNAMES",
        ] {
            env::remove_var(name);
        }
        Ok(Some(listener))
    }
}

fn socket_option(fd: BorrowedFd<'_>, level: i32, name: i32) -> Result<i32, i32> {
    let mut value: i32 = 0;
    let mut len = size_of::<i32>() as libc::socklen_t;
    // SAFETY: fd is borrowed and both option outputs are writable.
    retry(|| unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            level,
            name,
            (&raw mut value).cast(),
            &mut len,
        ) as isize
    })?;
    Ok(value)
}

fn inherited_address(listener: &TcpListener) -> Result<Address, i32> {
    let fd = listener.as_fd();
    if socket_option(fd, libc::SOL_SOCKET, libc::SO_TYPE)? != libc::SOCK_STREAM
        || !crate::host::sys::listening(fd.as_raw_fd())
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EINVAL))?
        || match crate::host::sys::PROTOCOL {
            Some(name) => socket_option(fd, libc::SOL_SOCKET, name)? != libc::IPPROTO_TCP,
            None => false,
        }
    {
        return Err(libc::EINVAL);
    }

    match listener
        .local_addr()
        .map_err(|e| e.raw_os_error().unwrap_or(libc::EINVAL))?
    {
        SocketAddr::V4(a) => Ok(Address {
            family: 0,
            hi: 0,
            lo: u64::from(u32::from(*a.ip())),
            port: a.port(),
        }),
        SocketAddr::V6(a) => {
            // A dual-stack wildcard also listens on IPv4, outside a V6 rule.
            if a.scope_id() != 0
                || (a.ip().is_unspecified()
                    && socket_option(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)? == 0)
            {
                return Err(libc::EACCES);
            }
            let ip = u128::from(*a.ip());
            Ok(Address {
                family: 1,
                hi: (ip >> 64) as u64,
                lo: ip as u64,
                port: a.port(),
            })
        }
    }
}

pub(crate) unsafe fn adopt_listener(request: *mut Request, fd: OwnedFd) -> Step {
    // SAFETY: only the OS LISTEN issue callback supplies a live request here.
    let request = unsafe { &mut *request };
    let n = unsafe { &mut *request.path.cast_mut().cast::<NetworkRequest>() };
    let listener = TcpListener::from(fd);
    let adopted = (|| {
        let address = inherited_address(&listener)?;
        if !unsafe {
            crate::net::permits_address(
                request.handle,
                &address,
                hirsel_core_net::AddressDirection::Listen,
            )
        } {
            return Err(libc::EACCES);
        }
        listener
            .set_nonblocking(true)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        let value = unsafe { register_socket(n, listener.into()) }?;
        n.address = address;
        Ok(value)
    })();
    let outcome = match adopted {
        Ok(value) => Outcome {
            value,
            ..Outcome::default()
        },
        Err(code) => error(code),
    };
    Step {
        outcome,
        ..Step::default()
    }
}

/// The platform launcher pins the image across exec, including the serving shell's
/// exec. Consume its descriptor before any grants or process metadata are opened.
pub(crate) fn receive_launch_image() -> io::Result<Option<[u8; 32]>> {
    let Some(value) = std::env::var_os(hirsel_core_ids::launch::LAUNCH_IMAGE_ENV) else {
        return Ok(None);
    };
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    let Some((parent, image)) = value.to_str().and_then(|text| text.split_once(':')) else {
        return Ok(None);
    };
    // SAFETY: getppid takes no arguments.
    if parent.parse::<libc::pid_t>().ok() != Some(unsafe { libc::getppid() }) {
        return Ok(None);
    }
    let (fd, digest) = image.split_once(':').ok_or_else(invalid)?;
    let fd = fd
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd >= 6)
        .ok_or_else(invalid)?;
    let digest = prepare_hex(digest.as_bytes())?;
    if !crate::host::sys::running_image(fd)? {
        return Err(invalid());
    }
    // SAFETY: the launch transferred this descriptor for the running image.
    drop(unsafe { OwnedFd::from_raw_fd(fd) });
    Ok(Some(digest))
}
