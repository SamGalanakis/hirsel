#[cfg(feature = "mimalloc")]
hirsel_guard::install!(mimalloc::MiMalloc: mimalloc::MiMalloc);
#[cfg(not(feature = "mimalloc"))]
hirsel_guard::install!();

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
    // a child tool inherits stdin, stdout and stderr only.
    hirsel_core_driver::close_inherited_descriptors();
    hirsel_core_driver::request::main(std::env::args().skip(1).collect())
}
