use std::path::Path;
use std::process::Command;

pub(crate) fn offline_editor(driver: &Path) -> Command {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.args([
        "-p",
        "(version 1) (allow default) (deny network*) (deny process-fork)",
    ]);
    command.arg(driver);
    command
}
