//! Run a candidate on an isolated Store image and print the Runner's migration report.

#[path = "dry_run/host/mod.rs"]
mod host;

use crate::cli::{Invocation, OptionId};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Error = Box<dyn std::error::Error>;

const REPORT: &[u8] = b"hirsel dry-run\n";

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    if invocation.positional.is_some() {
        return Err("dry-run takes the candidate through --exe".into());
    }
    let (mut executable, mut store) = (None, None);
    for (id, value) in invocation.options {
        let destination = match id {
            OptionId::DryRunExe => &mut executable,
            OptionId::Store => &mut store,
            _ => return Err("unsupported dry-run option".into()),
        };
        if destination.is_some() {
            return Err("dry-run takes one --exe and one --store".into());
        }
        *destination = value.map(PathBuf::from);
    }
    let executable = executable.ok_or("dry-run requires --exe NEW")?;
    let store = store.ok_or("dry-run requires --store DIR")?;
    let image = tempfile::tempdir()?;
    copy_store(&store, image.path())?;

    let mut report = tempfile::tempfile()?;
    let status = Command::new(std::path::absolute(executable)?)
        .arg("--hirsel-dry-run")
        .arg(std::path::absolute(image.path())?)
        .args(invocation.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(report.try_clone()?))
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .env_remove("LISTEN_PIDFDID")
        .status()?;
    report.rewind()?;
    let mut header = [0; REPORT.len()];
    if report.read_exact(&mut header).is_ok() && header == REPORT {
        io::copy(&mut report, &mut io::stdout().lock())?;
    } else if status.success() {
        return Err("the executable did not report a dry run".into());
    }
    Ok(status.code().unwrap_or(1))
}

fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn copy_store(source: &Path, image: &Path) -> Result<(), Error> {
    // An ordinary writer holds LOCK for its whole open. Its release is
    // required before capturing a consistent manifest, packs and journal.
    let lock = open_file(&source.join("LOCK"))?;
    match lock.try_lock_shared() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err("the store directory is held by another process (its LOCK)".into());
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(
                format!("unexpected entry in the store: {}", entry.path().display()).into(),
            );
        }
        let mut from = open_file(&entry.path())?;
        host::clone_file(&mut from, &image.join(entry.file_name()))?;
    }
    Ok(())
}
