//! Run a release on a store and print its report: `hirsel dry-run` and
//! `hirsel store verify --release` on an isolated image of the store,
//! `hirsel store repair --release` on the store itself (STORE-SPEC §15.6).

use crate::cli::{Invocation, OptionId};
use crate::native_entry::launch_config;
use crate::package_content::Sha256Writer;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
};

type Error = Box<dyn std::error::Error>;

/// Exit status for a store held by a running writer, as `store backup` uses.
const HELD: i32 = 75;
const NOT_A_PLACE: i32 = 104;

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
        "tapes check" => (
            "--hirsel-tapes-check",
            b"hirsel tapes check\n",
            "tapes check DIR --recorded EXE --release EXE",
        ),
        _ => (
            "--hirsel-store-repair",
            b"hirsel store repair\n",
            "store repair DIR --release EXE",
        ),
    };
    let (mut executable, mut store) = (None, invocation.positional.map(PathBuf::from));
    let mut recorded = None;
    if name == "dry-run" && store.is_some() {
        return Err("dry-run takes the candidate through --exe".into());
    }
    for (id, value) in invocation.options {
        let destination = match id {
            OptionId::DryRunExe | OptionId::StoreRelease => &mut executable,
            OptionId::RecordedExecutable if name == "tapes check" => &mut recorded,
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
    let executable = std::path::absolute(executable)?;
    let file = open_executable(&executable)?;
    if !file.metadata()?.is_file() || !launch_config::is_place(&file) {
        eprintln!("hirsel: {} is not a place", executable.display());
        return Ok(NOT_A_PLACE);
    }
    let witness = if name == "tapes check" {
        let recorded = recorded.ok_or_else(|| format!("usage: hirsel {usage}"))?;
        Some(witness_pair(&recorded, &file)?)
    } else {
        None
    };
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
    let launch_flag = witness
        .as_ref()
        .map_or_else(|| flag.to_owned(), |(digest, _)| format!("{flag}={digest}"));
    let status = crate::host::release_command(&file, &executable)
        .arg(launch_flag)
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
        if let Some((_, identity)) = witness {
            println!("{identity}");
        }
        io::copy(&mut report, &mut io::stdout().lock())?;
    } else if status.success() {
        return Err(format!("the executable did not report a {name}").into());
    }
    Ok(status.code().unwrap_or(1))
}

fn open_executable(path: &Path) -> Result<File, Error> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?)
}

fn witness_pair(recorded: &Path, witness: &File) -> Result<(String, serde_json::Value), Error> {
    let writer = open_executable(recorded)?;
    if !writer.metadata()?.is_file() {
        return Err(format!("{} is not a file", recorded.display()).into());
    }
    let (writer_backend, writer_revision) = launch_config::witness_identity(&writer)
        .ok_or("the recorded executable has no Place compiler identity")?;
    let (witness_backend, witness_revision) = launch_config::witness_identity(witness)
        .ok_or("the witness executable has no Place compiler identity")?;
    if writer_revision != witness_revision {
        return Err("the recorded and witness code revisions differ".into());
    }
    if writer_backend == witness_backend {
        return Err("the witness must use the other compiler backend".into());
    }
    let digest = |mut file: &File| -> Result<String, Error> {
        file.rewind()?;
        let mut hash = Sha256Writer::default();
        io::copy(&mut file, &mut hash)?;
        Ok(hirsel_core_ids::hex::encode(&hash.finish()))
    };
    let writer_digest = digest(&writer)?;
    let identity = serde_json::json!({
        "revision": hirsel_core_ids::hex::encode(&writer_revision),
        "recorded_backend": writer_backend,
        "recorded_image": writer_digest,
        "witness_backend": witness_backend,
        "witness_image": digest(witness)?,
    });
    Ok((writer_digest, identity))
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
        crate::host::clone_file(&mut from, &image.join(entry.file_name()))?;
    }
    Ok(true)
}
