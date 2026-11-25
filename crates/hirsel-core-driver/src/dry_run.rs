//! Run a release on a store and print its report: `hirsel dry-run` and
//! `hirsel store verify --release` on an isolated image of the store,
//! `hirsel store repair --release` on the store itself (STORE-SPEC §15.6).

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

/// Exit status for a store held by a running writer, as `store backup` uses.
const HELD: i32 = 75;

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    let name = invocation.command.name;
    let (flag, header, usage): (&str, &[u8], &str) = match name {
        "dry-run" => (
            "--hirsel-dry-run",
            b"hirsel dry-run\n",
            "dry-run --exe NEW --store DIR",
        ),
        "store verify" => (
            "--hirsel-store-verify",
            b"hirsel store verify\n",
            "store verify DIR --release EXE",
        ),
        _ => (
            "--hirsel-store-repair",
            b"hirsel store repair\n",
            "store repair DIR --release EXE",
        ),
    };
    let (mut executable, mut store) = (None, invocation.positional.map(PathBuf::from));
    if name == "dry-run" && store.is_some() {
        return Err("dry-run takes the candidate through --exe".into());
    }
    for (id, value) in invocation.options {
        let destination = match id {
            OptionId::DryRunExe | OptionId::StoreRelease => &mut executable,
            OptionId::Store if name == "dry-run" => &mut store,
            _ => return Err(format!("usage: hirsel {usage}").into()),
        };
        if destination.is_some() {
            return Err(format!("usage: hirsel {usage}").into());
        }
        *destination = value.map(PathBuf::from);
    }
    let (Some(executable), Some(store)) = (executable, store) else {
        return Err(format!("usage: hirsel {usage}").into());
    };
    if name != "dry-run" && !invocation.arguments.is_empty() {
        return Err(format!("usage: hirsel {usage}").into());
    }
    // A dry run and a verify read a clone and never write the store. A repair
    // writes it in place, so it needs the store stopped.
    let image = tempfile::tempdir()?;
    let target = if name == "store repair" {
        match unheld(&store)? {
            true => std::path::absolute(&store)?,
            false => {
                eprintln!("hirsel store: the store is held by a writer");
                return Ok(HELD);
            }
        }
    } else {
        if !copy_store(&store, image.path())? {
            eprintln!("hirsel store: the store is held by a writer");
            return Ok(HELD);
        }
        std::path::absolute(image.path())?
    };

    let mut report = tempfile::tempfile()?;
    let status = Command::new(std::path::absolute(executable)?)
        .arg(flag)
        .arg(target)
        .args(invocation.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(report.try_clone()?))
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .env_remove("LISTEN_PIDFDID")
        .status()?;
    report.rewind()?;
    let mut seen = vec![0; header.len()];
    if report.read_exact(&mut seen).is_ok() && seen == header {
        io::copy(&mut report, &mut io::stdout().lock())?;
    } else if status.success() {
        return Err(format!("the executable did not report a {name}").into());
    }
    Ok(status.code().unwrap_or(1))
}

fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Whether no writer holds the store's LOCK. An ordinary writer holds it for
/// its whole open; the shared lock taken here is released on return.
fn unheld(source: &Path) -> Result<bool, Error> {
    let lock = open_file(&source.join("LOCK"))?;
    match lock.try_lock_shared() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Clones the store into `image`, holding its LOCK shared so that no writer
/// opens it meanwhile. False when a writer holds it: a consistent manifest,
/// packs and journal need the writer gone.
pub(in crate::request) fn copy_store(source: &Path, image: &Path) -> Result<bool, Error> {
    let lock = open_file(&source.join("LOCK"))?;
    match lock.try_lock_shared() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
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
    Ok(true)
}
