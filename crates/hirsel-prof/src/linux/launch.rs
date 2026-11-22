use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr;

fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [0; 2];
    // SAFETY: pipe2 writes two valid, newly owned descriptors on success.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded and ownership of both descriptors transfers to File.
    Ok(unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    })
}

pub(crate) struct SuspendedLaunchedProcess {
    pid: libc::pid_t,
    // Keeping the read end here avoids SIGPIPE if the child exits early.
    resume: Option<(File, File)>,
    exec_error: File,
    reaped: bool,
}

impl SuspendedLaunchedProcess {
    pub(crate) fn fork(
        path: &Path,
        arguments: &[String],
        record: Option<&OsStr>,
    ) -> io::Result<Self> {
        let program = CString::new(path.as_os_str().as_bytes())?;
        let arguments = arguments
            .iter()
            .map(|arg| CString::new(arg.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let argv: Vec<_> = std::iter::once(program.as_ptr())
            .chain(arguments.iter().map(|arg| arg.as_ptr()))
            .chain(std::iter::once(ptr::null()))
            .collect();

        let mut env = Vec::new();
        for (key, value) in std::env::vars_os() {
            if record.is_some() && key == "HIRSEL_RECORD" {
                continue;
            }
            let mut entry = key.into_encoded_bytes();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            env.push(CString::new(entry)?);
        }
        if let Some(record) = record {
            let mut entry = b"HIRSEL_RECORD=".to_vec();
            entry.extend_from_slice(record.as_bytes());
            env.push(CString::new(entry)?);
        }
        let envp: Vec<_> = env
            .iter()
            .map(|value| value.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        let (resume_read, resume_write) = pipe()?;
        let (error_read, error_write) = pipe()?;

        // SAFETY: the child calls only async-signal-safe libc routines and
        // never returns to Rust drop glue, allocation, or the caller.
        let pid = unsafe {
            let pid = libc::fork();
            if pid == 0 {
                libc::close(resume_write.as_raw_fd());
                libc::close(error_read.as_raw_fd());
                exec_after_resume(
                    resume_read.as_raw_fd(),
                    error_write.as_raw_fd(),
                    program.as_ptr(),
                    argv.as_ptr(),
                    envp.as_ptr(),
                );
            }
            pid
        };
        if pid == -1 {
            return Err(io::Error::last_os_error());
        }
        drop(error_write);
        Ok(Self {
            pid,
            resume: Some((resume_read, resume_write)),
            exec_error: error_read,
            reaped: false,
        })
    }

    pub(crate) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    pub(crate) fn resume(&mut self) -> io::Result<()> {
        let (_read_end, mut write_end) = self.resume.take().expect("child resumes once");
        write_end.write_all(&[1])?;
        drop(write_end);
        let mut errno = [0; size_of::<i32>()];
        match self.exec_error.read(&mut errno[..1])? {
            0 => Ok(()), // Successful exec closed the FD.
            _ => {
                self.exec_error.read_exact(&mut errno[1..])?;
                Err(io::Error::from_raw_os_error(i32::from_ne_bytes(errno)))
            }
        }
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait(libc::WNOHANG)
    }

    fn wait(&mut self, options: libc::c_int) -> io::Result<Option<ExitStatus>> {
        loop {
            let mut status = 0;
            // SAFETY: status is writable and pid identifies our own fork child.
            match unsafe { libc::waitpid(self.pid, &mut status, options) } {
                0 => return Ok(None),
                -1 => {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
                _ => {
                    self.reaped = true;
                    return Ok(Some(ExitStatus::from_raw(status)));
                }
            }
        }
    }
}

impl Drop for SuspendedLaunchedProcess {
    fn drop(&mut self) {
        // EOF tells an unresumed child to exit without executing the program.
        if self.resume.take().is_some() {
            let _ = self.wait(0);
        } else if !self.reaped {
            let _ = self.try_wait();
        }
    }
}

unsafe fn exec_after_resume(
    resume: RawFd,
    error: RawFd,
    program: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> ! {
    // SAFETY: the caller's exec vectors are terminated and live, the fds are
    // owned, and every attempt to read and write uses a live, fixed-size buffer.
    unsafe {
        let mut command = 0u8;
        loop {
            match libc::read(resume, (&raw mut command).cast(), 1) {
                1 if command == 1 => break,
                -1 if *libc::__errno_location() == libc::EINTR => continue,
                _ => libc::_exit(125),
            }
        }
        libc::close(resume);
        libc::execve(program, argv, envp);
        let errno = *libc::__errno_location();
        // Writes below PIPE_BUF are atomic. Retry only an interrupted write.
        while libc::write(error, (&raw const errno).cast(), size_of::<i32>()) < 0
            && *libc::__errno_location() == libc::EINTR
        {}
        libc::_exit(127)
    }
}
