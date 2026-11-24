hirsel_guard::install!(mimalloc::MiMalloc: mimalloc::MiMalloc);

fn main() {
    // Every write to stdout or stderr meets a reader that may close early
    // (`hirsel explain --list | head`). Rust's startup ignores SIGPIPE, which
    // turns each such write into a panic; the default action ends the process
    // quietly with the SIGPIPE status. A program run holds its own policy while
    // it runs (hirsel-core-rt `PipePolicy`), and spawned tools start with the
    // default action.
    // SAFETY: no other thread exists yet, and SIG_DFL installs no handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    // A descriptor this process inherited open across exec (a shell's `3>`)
    // would pass on to every tool it starts. Mark them all close-on-exec, so
    // a child tool inherits stdin, stdout and stderr only. A kernel without
    // close_range (before 5.11) leaves them as they are.
    // SAFETY: close_range only changes descriptor flags; it touches no memory.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    // Without close_range, mark each descriptor /dev/fd lists.
    #[cfg(not(target_os = "linux"))]
    if let Ok(entries) = std::fs::read_dir("/dev/fd") {
        let fds: Vec<i32> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .filter(|&fd| fd > 2)
            .collect();
        for fd in fds {
            // SAFETY: F_SETFD only changes a descriptor flag; a descriptor
            // already closed (the listing's own) answers EBADF.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
    hirsel_core_driver::request::main(std::env::args().skip(1).collect())
}
