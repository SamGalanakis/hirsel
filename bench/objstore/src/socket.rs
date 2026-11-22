use io_uring::{IoUring, cqueue, opcode, squeue, types};
use std::io::{self, IoSlice, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub const CHUNK: usize = 256 * 1024;

pub struct Epoll {
    pub stream: TcpStream,
    epoll: OwnedFd,
}

impl Epoll {
    pub fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        // SAFETY: a successful epoll_create1 returns a new, owned descriptor.
        let epoll =
            unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) };
        let mut interest = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32,
            u64: 0,
        };
        // SAFETY: interest describes one event and both descriptors are live.
        checked(unsafe {
            libc::epoll_ctl(
                epoll.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                stream.as_raw_fd(),
                &mut interest,
            )
        })?;
        Ok(Self { stream, epoll })
    }

    fn retry(&self, mut call: impl FnMut(RawFd) -> isize) -> io::Result<usize> {
        let fd = self.stream.as_raw_fd();
        loop {
            let result = call(fd);
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {}
                _ => return Err(error),
            }

            let mut event = libc::epoll_event { events: 0, u64: 0 };
            // SAFETY: event is writable for one event and epoll is live.
            let ready = unsafe { libc::epoll_wait(self.epoll.as_raw_fd(), &mut event, 1, -1) };
            match checked(ready) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => {
                    result?;
                }
            }
        }
    }
}

impl Read for Epoll {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: buf is writable for its stated length during recv.
        self.retry(|fd| unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) })
    }
}

impl Write for Epoll {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: buf is readable for its stated length during send.
        self.retry(|fd| unsafe {
            libc::send(fd, buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL)
        })
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        // SAFETY: a zeroed msghdr has no ancillary data. IoSlice has the Unix
        // iovec ABI, and each slice is readable for the duration of sendmsg.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = bufs.as_ptr().cast_mut().cast();
        message.msg_iovlen = bufs.len();
        self.retry(|fd| unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Ring {
    ring: IoUring,
    stream: TcpStream,
    app_rx: libc::iovec,
    app_tx: libc::iovec,
    rx: Box<[u8]>,
    tx: Box<[u8]>,
    send_zc: bool,
    pub zc_bytes: u64,
    pub copied_bytes: u64,
}

fn iovec(slice: &mut [u8]) -> libc::iovec {
    libc::iovec {
        iov_base: slice.as_mut_ptr().cast(),
        iov_len: slice.len(),
    }
}

fn contains(range: libc::iovec, start: *const u8, len: usize) -> bool {
    (start as usize)
        .checked_sub(range.iov_base as usize)
        .is_some_and(|offset| offset <= range.iov_len && len <= range.iov_len - offset)
}

impl Ring {
    /// The caller keeps the application buffers at these addresses until Ring
    /// is dropped. Transfers borrow the addressed bytes until their last CQE.
    pub unsafe fn new(
        stream: TcpStream,
        app_rx: &mut [u8],
        app_tx: &mut [u8],
        send_zc: bool,
    ) -> io::Result<Self> {
        stream.set_nonblocking(false)?;
        let ring = IoUring::builder()
            .setup_single_issuer()
            .setup_coop_taskrun()
            .setup_defer_taskrun()
            .build(8)?;
        let mut this = Self {
            ring,
            stream,
            app_rx: iovec(app_rx),
            app_tx: iovec(app_tx),
            rx: vec![0; CHUNK].into_boxed_slice(),
            tx: vec![0; CHUNK].into_boxed_slice(),
            send_zc,
            zc_bytes: 0,
            copied_bytes: 0,
        };
        let fixed = [
            this.app_rx,
            this.app_tx,
            iovec(&mut this.rx),
            iovec(&mut this.tx),
        ];
        // SAFETY: the owned staging buffers and the caller's fixed buffers
        // survive the ring. Neither the staging nor the app buffers move.
        unsafe { this.ring.submitter().register_buffers(&fixed)? };
        Ok(this)
    }

    fn start(&mut self, entry: squeue::Entry) -> cqueue::Entry {
        // SAFETY: the calling read/write keeps its buffer live through every
        // CQE. The ring has room because it drains each submitted operation.
        unsafe { self.ring.submission().push(&entry).expect("one SQE fits") };
        self.complete()
    }

    fn complete(&mut self) -> cqueue::Entry {
        loop {
            if let Some(cqe) = self.ring.completion().next() {
                return cqe;
            }
            match self.ring.submit_and_wait(1) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    // The published SQE may still own the caller's borrowed
                    // buffer. Exit without unwinding or freeing its memory.
                    eprintln!("io_uring_enter after publishing SQE: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn cqe_result(cqe: &cqueue::Entry) -> io::Result<usize> {
    if cqe.result() < 0 {
        Err(io::Error::from_raw_os_error(-cqe.result()))
    } else {
        Ok(cqe.result() as usize)
    }
}

impl Read for Ring {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(CHUNK);
        let (ptr, fixed) = match contains(self.app_rx, buf.as_ptr(), len) {
            true => (buf.as_mut_ptr(), 0),
            false => (self.rx.as_mut_ptr(), 2),
        };
        let entry =
            opcode::ReadFixed::new(types::Fd(self.stream.as_raw_fd()), ptr, len as u32, fixed)
                .build();
        let n = cqe_result(&self.start(entry))?;
        if fixed == 2 {
            buf[..n].copy_from_slice(&self.rx[..n]);
        }
        Ok(n)
    }
}

impl Write for Ring {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(CHUNK);
        let fd = types::Fd(self.stream.as_raw_fd());
        if !self.send_zc {
            let entry = opcode::Send::new(fd, buf.as_ptr(), len as u32)
                .flags(libc::MSG_NOSIGNAL)
                .build();
            return cqe_result(&self.start(entry));
        }
        let (ptr, fixed) = match contains(self.app_tx, buf.as_ptr(), len) {
            true => (buf.as_ptr(), 1),
            false => {
                self.tx[..len].copy_from_slice(&buf[..len]);
                (self.tx.as_ptr(), 3)
            }
        };
        let entry = opcode::SendZc::new(fd, ptr, len as u32)
            .buf_index(Some(fixed))
            .flags(libc::MSG_NOSIGNAL)
            .zc_flags(8 /* IORING_SEND_ZC_REPORT_USAGE */)
            .build();
        let cqe = self.start(entry);
        let result = cqe_result(&cqe);
        if cqueue::more(cqe.flags()) {
            let notification = self.complete();
            assert!(
                cqueue::notif(notification.flags()),
                "expected SEND_ZC notification"
            );
            if let Ok(n) = result {
                if notification.result() as u32 & (1 << 31) /* IORING_NOTIF_USAGE_ZC_COPIED */ != 0
                {
                    self.copied_bytes += n as u64;
                } else {
                    self.zc_bytes += n as u64;
                }
            }
        } else if let Ok(n) = result {
            self.copied_bytes += n as u64;
        }
        result.map_err(|error| {
            if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
                io::Error::other(format!("SEND_ZC refused: {error}"))
            } else {
                error
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn checked(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}
