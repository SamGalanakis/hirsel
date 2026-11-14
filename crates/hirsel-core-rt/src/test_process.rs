//! Reexecute native libtest children through the explicitly selected ARM runner.
use std::process::Command;

pub(crate) fn command() -> Command {
    let executable = std::env::current_exe().unwrap();
    #[cfg(target_arch = "aarch64")]
    {
        let runner = std::env::var_os("HIRSEL_QEMU_AARCH64")
            .expect("ARM runtime tests require an explicit HIRSEL_QEMU_AARCH64");
        let sysroot = std::env::var_os("HIRSEL_AARCH64_SYSROOT")
            .expect("ARM runtime tests require an explicit HIRSEL_AARCH64_SYSROOT");
        let mut command = Command::new(runner);
        command.arg("-L").arg(sysroot).arg(executable);
        command
    }
    #[cfg(not(target_arch = "aarch64"))]
    Command::new(executable)
}
