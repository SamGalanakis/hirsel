//! `hirsel store backup DIR TARGET` and `hirsel store restore BACKUP DIR`
//! (STORE-SPEC §15.4), and `hirsel store inspect DIR`, `hirsel store stats DIR` and
//! `hirsel store verify DIR` (§15.6): open the directories and run the installed
//! `libexec/hirsel/store_tool` with them as fds 3 and 4. Backup and restore create
//! the second directory if it is absent. Inspect and stats read the store itself
//! beside a live writer; verify scrubs a clone. Those three pass one directory as both.

use super::dry_run::copy_store;
use crate::cli::Invocation;
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    process::{Command, Stdio},
};

type Error = Box<dyn std::error::Error>;

fn open_dir(path: impl AsRef<std::path::Path>) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    let (verb, usage) = match invocation.command.name {
        "store backup" => ("backup", "store backup DIR TARGET"),
        "store restore" => ("restore", "store restore BACKUP DIR"),
        "store inspect" => ("inspect", "store inspect DIR"),
        "store stats" => ("stats", "store stats DIR"),
        "store verify" => ("verify", "store verify DIR [--release EXE]"),
        _ => return Err("usage: hirsel store <inspect|stats|verify|repair|backup|restore>".into()),
    };
    if !invocation.options.is_empty() {
        return Err(format!("usage: hirsel {usage}").into());
    }
    let Some(from) = invocation.positional.as_deref() else {
        return Err(format!("usage: hirsel {usage}").into());
    };
    if matches!(verb, "inspect" | "stats" | "verify") {
        if !invocation.arguments.is_empty() {
            return Err(format!("usage: hirsel {usage}").into());
        }
        let image = tempfile::tempdir()?;
        let source = if verb == "verify" {
            if !copy_store(std::path::Path::new(from), image.path())? {
                eprintln!("hirsel store: the store is held by a writer");
                return Ok(75);
            }
            open_dir(image.path())?
        } else {
            open_dir(from)?
        };
        let same = source.try_clone()?;
        return store_tool(verb, source, same);
    }
    let [to] = &invocation.arguments[..] else {
        return Err(format!("usage: hirsel {usage}").into());
    };
    let source = open_dir(from)?;
    match fs::create_dir(to) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let target = open_dir(to)?;
    store_tool(verb, source, target)
}

fn store_tool(verb: &str, source: File, target: File) -> Result<i32, Error> {
    let (source_fd, target_fd) = (source.as_raw_fd(), target.as_raw_fd());
    let mut command = Command::new(crate::operator_cli::installed("store_tool")?);
    command.arg(verb).stdin(Stdio::null());
    // SAFETY: only async-signal-safe fcntl and dup2 calls on descriptors this
    // process owns, between fork and exec. Both are first moved above 4 so that
    // placing one on 3 or 4 cannot overwrite the other; dup2 clears FD_CLOEXEC.
    unsafe {
        command.pre_exec(move || {
            let high_source = libc::fcntl(source_fd, libc::F_DUPFD, 10);
            let high_target = libc::fcntl(target_fd, libc::F_DUPFD, 10);
            if high_source < 0
                || high_target < 0
                || libc::dup2(high_source, 3) < 0
                || libc::dup2(high_target, 4) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command.status()?;
    drop((source, target));
    Ok(status.code().unwrap_or(1))
}
