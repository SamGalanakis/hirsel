//! Reexecute native libtest children: directly on the host's own architecture,
//! and through the explicitly selected ARM runner for aarch64 Linux under QEMU.
use std::{ffi::OsStr, process::Command};

pub(crate) fn command() -> Command {
    command_for(std::env::current_exe().unwrap().as_os_str())
}

pub(crate) fn command_for(executable: &OsStr) -> Command {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        let runner = std::env::var_os("HIRSEL_QEMU_AARCH64")
            .expect("ARM runtime tests require an explicit HIRSEL_QEMU_AARCH64");
        let sysroot = std::env::var_os("HIRSEL_AARCH64_SYSROOT")
            .expect("ARM runtime tests require an explicit HIRSEL_AARCH64_SYSROOT");
        let mut command = Command::new(runner);
        command.arg("-L").arg(sysroot).arg(executable);
        command
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    Command::new(executable)
}

/// Run one test in its own process. The parent returns true after the
/// child's test passes. The child returns false and runs the test body.
pub(crate) fn run_isolated(test: &str) -> bool {
    const CASE: &str = "HIRSEL_TEST_ISOLATED_CASE";
    if std::env::var_os(CASE).as_deref() == Some(OsStr::new(test)) {
        return false;
    }
    let output = command()
        .args(["--exact", test, "--format=terse"])
        .env(CASE, test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "{test}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
