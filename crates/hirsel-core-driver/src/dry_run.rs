//! Run a release on a store and print its report: `hirsel store rehearse` and
//! `hirsel store verify --release` on an isolated image of the store,
//! `hirsel store repair --release` on the store itself (STORE-SPEC §15.6).

use crate::cli::{Request, StoreOption as Opt};
use crate::native_entry::launch_config;
use crate::package_content::Sha256Writer;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read, Seek},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
};

type Error = Box<dyn std::error::Error>;

/// Exit status for a store held by a running writer, as `store backup` uses.
const HELD: i32 = 75;
const NOT_A_PLACE: i32 = 104;

pub(in crate::request) fn run(invocation: Request<Opt>) -> Result<i32, Error> {
    let name = invocation.command.name;
    let machine = invocation.options.contains(&Opt::Json);
    let mut env = crate::operator_cli::finite::Envelope::read(name);
    env.contract = "execute";
    let result = execute(invocation, machine.then_some(&mut env));
    if !machine {
        return result;
    }
    use crate::operator_cli::finite::Failure;
    match result {
        Ok(0) => (),
        Ok(75) => env.fail(Failure::blocked("Held", "the store is held by a writer")),
        Ok(NOT_A_PLACE) => env.fail(Failure::input("release", "the executable is not a Place")),
        Ok(_) => env.fail(Failure::negative(
            "CheckFailed",
            "the release reported a failed check",
        )),
        Err(error) => env.fail(Failure::input("arguments", error)),
    }
    Ok(env.render(true))
}

fn execute(
    invocation: Request<Opt>,
    machine: Option<&mut crate::operator_cli::finite::Envelope<'_>>,
) -> Result<i32, Error> {
    let name = invocation.command.name;
    // These markers are the installed release protocol. Public command names can change independently.
    let (flag, header, usage): (&str, &[u8], &str) = match name {
        "store rehearse" => (
            "--hirsel-dry-run",
            b"hirsel dry-run\n",
            "store rehearse DIR --release EXE",
        ),
        "store verify" => (
            "--hirsel-store-verify",
            b"hirsel store verify\n",
            "store verify DIR --release EXE",
        ),
        "tape check" => (
            "--hirsel-tapes-check",
            b"hirsel tapes check\n",
            "tape check DIR --recorded EXE --release EXE",
        ),
        _ => (
            "--hirsel-store-repair",
            b"hirsel store repair\n",
            "store repair DIR --release EXE",
        ),
    };
    let (mut executable, store) = (None, invocation.positional.map(PathBuf::from));
    let mut recorded = None;
    for option in invocation.options {
        let (destination, value) = match option {
            Opt::Json => continue,
            Opt::StoreRelease(value) => (&mut executable, value),
            Opt::RecordedExecutable(value) if name == "tape check" => (&mut recorded, value),
            _ => return Err(format!("usage: hirsel {usage}").into()),
        };
        if destination.is_some() {
            return Err(format!("usage: hirsel {usage}").into());
        }
        *destination = Some(PathBuf::from(value));
    }
    let (Some(executable), Some(store)) = (executable, store) else {
        return Err(format!("usage: hirsel {usage}").into());
    };
    if name != "store rehearse" && !invocation.arguments.is_empty() {
        return Err(format!("usage: hirsel {usage}").into());
    }
    let executable = std::path::absolute(executable)?;
    let file = open_executable(&executable)?;
    if !file.metadata()?.is_file() || !launch_config::is_place(&file) {
        eprintln!("hirsel: {} is not a place", executable.display());
        return Ok(NOT_A_PLACE);
    }
    let witness = if name == "tape check" {
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
        if let Some(env) = machine {
            if let Some((_, identity)) = witness {
                let mut summary = None;
                for line in io::BufReader::new(report).lines() {
                    let line: serde_json::Value = serde_json::from_str(&line?)?;
                    if line["through"].is_u64() {
                        summary = Some(line);
                    }
                }
                let mut coverage = summary.ok_or("the witness did not report journal coverage")?;
                for value in coverage.as_object_mut().unwrap().values_mut() {
                    let number = value.as_u64().ok_or("invalid tape check count")?;
                    *value = serde_json::json!(number.to_string());
                }
                env.result = serde_json::json!({"witness":identity, "coverage":coverage});
            } else {
                let mut text = String::new();
                report.read_to_string(&mut text)?;
                env.result = serde_json::json!({"report":text});
            }
        } else {
            if let Some((_, identity)) = witness {
                println!("{identity}");
            }
            io::copy(&mut report, &mut io::stdout().lock())?;
        }
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
