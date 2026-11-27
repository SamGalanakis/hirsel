//! The helper's side: the `main` of the shipped `hirsel-exec-helper`
//! executable. It serves the Supervisor on the socket at descriptor 3 and
//! ends when the embedder closes it (EMB-3).
use super::spawn::SOCKET_FD;
use super::wire::{self, Fields, Frame, Inbox};
use crate::host::exec::{Process, Signal, SpawnInput, Supervisor, Termination};
use hirsel_core_net::exec::ImagePin;
use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, mpsc};
use std::thread;

/// Exit status of a helper started without its socket, or one that could not
/// close the descriptors it inherited.
const UNUSABLE: libc::c_int = 2;

/// The helper's `main`. It reads no argument or environment variable.
pub fn main() -> ! {
    #[cfg(target_os = "linux")]
    // SAFETY: closes every descriptor past the socket. Darwin's spawn already
    // closed them (POSIX_SPAWN_CLOEXEC_DEFAULT).
    if unsafe { libc::syscall(libc::SYS_close_range, SOCKET_FD as u32 + 1, u32::MAX, 0u32) } != 0 {
        end(UNUSABLE);
    }
    if !is_stream_socket() {
        end(UNUSABLE);
    }
    // SAFETY: descriptor 3 is the stream socket checked above.
    serve(unsafe { OwnedFd::from_raw_fd(SOCKET_FD) });
    end(0)
}

/// Whether descriptor 3 is a Unix-domain stream socket.
fn is_stream_socket() -> bool {
    let mut kind: libc::c_int = 0;
    let mut len = mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: SO_TYPE writes one int.
    let typed = unsafe {
        libc::getsockopt(
            SOCKET_FD,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &mut len,
        )
    };
    // SAFETY: plain data, written by getsockname.
    let mut address: libc::sockaddr_storage = unsafe { mem::zeroed() };
    let mut len = mem::size_of_val(&address) as libc::socklen_t;
    // SAFETY: getsockname writes at most `len` bytes.
    let named = unsafe { libc::getsockname(SOCKET_FD, (&raw mut address).cast(), &mut len) };
    typed == 0
        && kind == libc::SOCK_STREAM
        && named == 0
        && libc::c_int::from(address.ss_family) == libc::AF_UNIX
}

fn end(status: libc::c_int) -> ! {
    // SAFETY: _exit ends the process.
    unsafe { libc::_exit(status) }
}

enum Request {
    Spawn { id: u64, input: SpawnInput },
    Signal { id: u64, signal: Signal },
    Release { id: u64 },
}

fn parse(inbox: &mut Inbox, tag: u8, bytes: &[u8]) -> io::Result<Request> {
    let invalid = || io::Error::from_raw_os_error(libc::EIO);
    let mut fields = Fields(bytes);
    Ok(match tag {
        wire::SPAWN => {
            let id = fields.u64()?;
            let mask = fields.u8()?;
            let pin = match fields.u8()? {
                0 => None,
                _ => Some(ImagePin::decode(
                    fields.bytes()?.try_into().map_err(|_| invalid())?,
                )),
            };
            let image = CString::new(fields.bytes()?).map_err(|_| invalid())?;
            let mut list = || -> io::Result<Vec<CString>> {
                let n = fields.u64()?;
                (0..n)
                    .map(|_| CString::new(fields.bytes()?).map_err(|_| invalid()))
                    .collect()
            };
            let argv = list()?;
            let envp = list()?;
            Request::Spawn {
                id,
                input: SpawnInput {
                    image: Arc::from(image.into_boxed_c_str()),
                    pin,
                    cwd: inbox.fd()?,
                    argv,
                    envp,
                    piped: [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0],
                },
            }
        }
        wire::SIGNAL => Request::Signal {
            id: fields.u64()?,
            signal: super::signal_of(fields.u8()?).ok_or_else(invalid)?,
        },
        wire::RELEASE => Request::Release { id: fields.u64()? },
        _ => return Err(invalid()),
    })
}

fn serve(socket: OwnedFd) {
    let fd = socket.as_raw_fd();
    #[cfg(target_vendor = "apple")]
    if wire::quiet(fd).is_err() {
        return;
    }
    let supervisor = match Supervisor::start(crate::exec::probe) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            let _ = Frame::new(wire::REFUSED)
                .bytes(error.to_string().as_bytes())
                .send(fd, &[]);
            return;
        }
    };
    if Frame::new(wire::READY)
        .bytes(wire::identity().as_bytes())
        .send(fd, &[])
        .is_err()
    {
        return;
    }
    // The reader hands requests over and wakes this thread; `None` is the
    // embedder's end.
    let (requests, inbox_rx) = mpsc::channel();
    let main = thread::current();
    let reader = thread::Builder::new()
        .name("hirsel-exec-helper".into())
        .spawn(move || {
            let mut inbox = Inbox::new(fd);
            loop {
                let request = match inbox.next() {
                    Ok(Some((tag, bytes))) => parse(&mut inbox, tag, &bytes).ok(),
                    Ok(None) | Err(_) => None,
                };
                let end = request.is_none();
                let _ = requests.send(request);
                main.unpark();
                if end {
                    return;
                }
            }
        });
    if reader.is_err() {
        return;
    }
    let mut children: HashMap<u64, Process> = HashMap::new();
    'serve: loop {
        // Every wake rescans every child, so readies already taken are spent.
        crate::tasks::ready::clear_alone();
        loop {
            match inbox_rx.try_recv() {
                Ok(Some(Request::Spawn { id, input })) => {
                    let sent = match supervisor.spawn(input) {
                        Ok(started) => {
                            let pipes = [started.stdin, started.stdout, started.stderr];
                            let mask = pipes
                                .iter()
                                .enumerate()
                                .fold(0u8, |mask, (at, fd)| mask | u8::from(fd.is_some()) << at);
                            let fds: Vec<_> =
                                pipes.iter().flatten().map(AsRawFd::as_raw_fd).collect();
                            children.insert(id, started.process);
                            Frame::new(wire::SPAWNED).u64(id).u8(mask).send(fd, &fds)
                        }
                        Err(error) => Frame::new(wire::SPAWN_FAILED)
                            .u64(id)
                            .u64(error.error.raw_os_error().unwrap_or(libc::EIO) as u64)
                            .send(fd, &[]),
                    };
                    if sent.is_err() {
                        break 'serve;
                    }
                }
                Ok(Some(Request::Signal { id, signal })) => {
                    if let Some(process) = children.get_mut(&id) {
                        let _ = process.signal(signal);
                    }
                }
                Ok(Some(Request::Release { id })) => {
                    children.remove(&id);
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => break 'serve,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let mut lost = false;
        children.retain(|&id, process| {
            let frame = match process.wait_or_park(Some(id)) {
                Ok(None) => return true,
                Ok(Some(Termination::Exited(code))) => {
                    Frame::new(wire::EXITED).u64(id).u8(0).u8(code)
                }
                Ok(Some(Termination::Signalled(number))) => {
                    Frame::new(wire::EXITED).u64(id).u8(1).u8(number)
                }
                Err(error) => Frame::new(wire::WAIT_FAILED)
                    .u64(id)
                    .u64(error.raw_os_error().unwrap_or(libc::EIO) as u64),
            };
            lost |= frame.send(fd, &[]).is_err();
            false
        });
        if lost {
            break;
        }
        thread::park();
    }
    // Releasing each child kills and reaps it; the Supervisor's drop then
    // ends its thread in order.
    drop(children);
    drop(supervisor);
    drop(socket);
}
