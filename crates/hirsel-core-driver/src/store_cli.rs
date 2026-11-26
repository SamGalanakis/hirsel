//! `hirsel store backup DIR TARGET`, `hirsel store backup --state DIR --group NAME TARGET`,
//! and `hirsel store restore BACKUP DIR` (STORE-SPEC §15.4), and `hirsel store inspect DIR`,
//! `hirsel store stats DIR` and `hirsel store verify DIR` (§15.6): open the directories and
//! run the installed `libexec/hirsel/store_tool` with them as fds 3 and 4. Online backup
//! routes through the running host's `STATE/operator.sock` so the serving place runs
//! `begin_backup`/`export_copy`/`release_export` without stopping, then materializes the
//! image into `TARGET` with `EXPORT` last.

use super::dry_run::copy_store;
use crate::cli::{Invocation, OptionId};
use crate::launcher::session::Group;
use crate::operator::{self, Answer, Ask, Class};
use hirsel_core_ids::hex;
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Error = Box<dyn std::error::Error>;

const UNAVAILABLE: i32 = 75;

fn open_dir(path: impl AsRef<Path>) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

fn ensure_empty_target(to: &Path) -> Result<bool, Error> {
    match fs::create_dir(to) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(fs::read_dir(to)?.next().is_none())
}

fn valid_export_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn scratch_dir_for(state: &Path, group: &Group, export_name: &str) -> PathBuf {
    state
        .join("scratch")
        .join(format!(
            "{}-{}",
            hex::encode(group.name.as_bytes()),
            group.incarnation
        ))
        .join(export_name)
}

fn move_or_copy_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
            fs::copy(from, to)?;
            let file = OpenOptions::new().read(true).open(to)?;
            file.sync_all()?;
            fs::remove_file(from)?;
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

/// Moves every file from the host's scratch backup directory `from` into the empty `to`
/// directory, moving `EXPORT` last and fsyncing `to` so an interrupted copy never looks
/// like a finished backup (STORE-SPEC §15.3/§15.4). Removes `from` when finished.
fn materialize_backup(from: &Path, to: &Path) -> io::Result<()> {
    let mut names = Vec::new();
    let mut has_export = false;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "EXPORT" {
            has_export = true;
        } else {
            names.push(name);
        }
    }
    if !has_export {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "backup image is missing EXPORT",
        ));
    }
    for name in names {
        move_or_copy_file(&from.join(&name), &to.join(&name))?;
    }
    open_dir(to)?.sync_all()?;
    move_or_copy_file(&from.join("EXPORT"), &to.join("EXPORT"))?;
    open_dir(to)?.sync_all()?;
    let _ = fs::remove_dir_all(from);
    Ok(())
}

fn online_backup(state: &Path, group_name: &str, to: &Path) -> Result<i32, Error> {
    if !ensure_empty_target(to)? {
        eprintln!("hirsel store: refused: Exists");
        return Ok(1);
    }
    let mut connection = match operator::connect(state) {
        Ok(conn) => conn,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            eprintln!(
                "hirsel store: temporarily unavailable: no host is listening at {}: {error}",
                state.display()
            );
            return Ok(UNAVAILABLE);
        }
        Err(error) => return Err(error.into()),
    };
    connection.send(&Ask::Backup(group_name.to_owned()))?;
    match connection.recv()? {
        Some(Answer::Backup(group, store_hi, store_lo, head, export_name)) => {
            if !valid_export_name(&export_name) {
                eprintln!("hirsel store: refused: ExportMismatch");
                return Ok(1);
            }
            let src = scratch_dir_for(state, &group, &export_name);
            materialize_backup(&src, to)?;
            println!("backup {store_hi:016x}{store_lo:016x} head {head}");
            Ok(0)
        }
        Some(Answer::BackupOffline(store_path)) => {
            let source = open_dir(store_path)?;
            let target = open_dir(to)?;
            store_tool("backup", source, target)
        }
        Some(Answer::Error(Class::Busy, detail)) => {
            if detail == "Busy" {
                eprintln!("hirsel store: refused: AttemptBusy");
            } else {
                eprintln!("hirsel store: refused: Busy: {detail}");
            }
            Ok(1)
        }
        Some(Answer::Error(_, detail)) => {
            eprintln!("hirsel store: refused: {detail}");
            Ok(1)
        }
        Some(other) => Err(format!("hirsel store backup: unexpected reply {other:?}").into()),
        None => {
            eprintln!("hirsel store: temporarily unavailable: host closed the connection");
            Ok(UNAVAILABLE)
        }
    }
}

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    let (verb, usage) = match invocation.command.name {
        "store backup" => (
            "backup",
            "store backup DIR TARGET | store backup --state DIR --group NAME TARGET",
        ),
        "store restore" => ("restore", "store restore BACKUP DIR"),
        "store inspect" => ("inspect", "store inspect DIR"),
        "store stats" => ("stats", "store stats DIR"),
        "store verify" => ("verify", "store verify DIR [--release EXE]"),
        _ => return Err("usage: hirsel store <inspect|stats|verify|repair|backup|restore>".into()),
    };
    if verb == "backup" && !invocation.options.is_empty() {
        let mut state: Option<PathBuf> = None;
        let mut group: Option<String> = None;
        for (id, value) in invocation.options {
            match id {
                OptionId::HostState => state = value.map(PathBuf::from),
                OptionId::GroupName => group = value,
                _ => return Err(format!("usage: hirsel {usage}").into()),
            }
        }
        let (Some(state), Some(group), Some(to)) = (state, group, invocation.positional) else {
            return Err(format!("usage: hirsel {usage}").into());
        };
        if !invocation.arguments.is_empty() {
            return Err(format!("usage: hirsel {usage}").into());
        }
        return online_backup(&state, &group, Path::new(&to));
    }
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
            if !copy_store(Path::new(from), image.path())? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Action, parse};

    #[test]
    fn store_backup_parses_online_and_offline_forms() {
        let Action::Invoke(online) = parse([
            "store".into(),
            "backup".into(),
            "--state".into(),
            "/var/state".into(),
            "--group".into(),
            "app".into(),
            "/tmp/out".into(),
        ])
        .unwrap() else {
            panic!("expected Invoke");
        };
        assert_eq!(online.command.name, "store backup");
        assert_eq!(online.positional.as_deref(), Some("/tmp/out"));
        assert!(online.arguments.is_empty());
        assert_eq!(online.options.len(), 2);

        let Action::Invoke(offline) = parse([
            "store".into(),
            "backup".into(),
            "/var/store".into(),
            "/tmp/out".into(),
        ])
        .unwrap() else {
            panic!("expected Invoke");
        };
        assert_eq!(offline.positional.as_deref(), Some("/var/store"));
        assert_eq!(offline.arguments, vec!["/tmp/out"]);
        assert!(offline.options.is_empty());
    }

    #[test]
    fn materialize_backup_moves_files_and_refuses_missing_export() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src.join("MANIFEST"), b"manifest-bytes").unwrap();
        fs::write(src.join("j-1"), b"journal-bytes").unwrap();
        assert!(materialize_backup(&src, &dst).is_err());

        fs::write(src.join("EXPORT"), b"export-bytes").unwrap();
        materialize_backup(&src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(dst.join("MANIFEST")).unwrap(), b"manifest-bytes");
        assert_eq!(fs::read(dst.join("j-1")).unwrap(), b"journal-bytes");
        assert_eq!(fs::read(dst.join("EXPORT")).unwrap(), b"export-bytes");
    }
}
