//! Files that are executed after they are written.
//!
//! Executing a file fails with ETXTBSY while any process holds it open for
//! writing. A writable descriptor opened in a multithreaded process is copied
//! into the child of every fork another thread makes before the child execs,
//! so a file written and then executed at once can be busy for a moment that
//! no `O_CLOEXEC` closes. The writer therefore runs on a thread with its own
//! descriptor table: no other thread's fork can copy its descriptor.
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Publish `bytes` at `path` with `mode`, atomically, from a private
/// descriptor table: no process other than this writer ever holds the file
/// open for writing.
pub(crate) fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                // SAFETY: unshare(CLONE_FILES) gives this thread a private copy
                // of the descriptor table; no memory is involved, and the table
                // is released when the thread exits.
                #[cfg(target_os = "linux")]
                if unsafe { libc::unshare(libc::CLONE_FILES) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                file.write_all(bytes)?;
                file.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(mode))?;
                file.persist(path).map_err(|e| e.error)?;
                Ok(())
            })
            .join()
            .map_err(|_| io::Error::other("the executable writer panicked"))?
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A file the driver writes runs at once while another thread forks: the
    /// fork never copies the writer's descriptor (no ETXTBSY).
    #[test]
    fn a_written_executable_runs_while_another_thread_forks() {
        let dir = tempfile::tempdir().unwrap();
        // macOS kills a copy of a platform binary such as /usr/bin/true.
        let program = if cfg!(target_os = "macos") {
            b"#!/bin/sh\nexit 0\n".to_vec()
        } else {
            std::fs::read("/bin/true").unwrap()
        };
        let stop = AtomicBool::new(false);
        let busy = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    Command::new("true").status().unwrap();
                }
            });
            let mut busy = 0;
            for i in 0..1000 {
                let path = dir.path().join(format!("p{i}"));
                super::write(&path, &program, 0o755).unwrap();
                match Command::new(&path).status() {
                    Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => busy += 1,
                    status => assert!(status.unwrap().success()),
                }
                std::fs::remove_file(&path).unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            busy
        });
        assert_eq!(busy, 0, "runs refused with ETXTBSY");
    }
}
