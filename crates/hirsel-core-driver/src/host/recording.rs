use super::{Error, Records, attempt};
use crate::cli::{Invocation, OptionId as Opt};
use hirsel_core_ids::ContentHash;
use hirsel_tape_format::{Corrupt, Cursor, KIND_RUN};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct Recording {
    pub(super) id: [u8; 16],
    pub(super) path: PathBuf,
}

impl Recording {
    pub(super) fn new(directory: &Path) -> io::Result<Self> {
        let id = attempt()?;
        let path = directory.join(format!("{}.tape", crate::call::hex(&id)));
        Ok(Self { id, path })
    }

    pub(super) fn attach(&self, command: &mut Command) {
        command
            .env("HIRSEL_RECORD", &self.path)
            .env("HIRSEL_RECORD_ID", crate::call::hex(&self.id));
    }
}

pub(super) fn unix_seconds() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(io::Error::other)
}

pub(crate) fn replay(invocation: Invocation) -> Result<i32, Error> {
    if invocation.positional.is_some() || !invocation.arguments.is_empty() {
        return Err("host recording replay takes its program and arguments from the pin".into());
    }
    let mut state = None;
    let mut tape = None;
    let mut diagnose = false;
    for (option, value) in invocation.options {
        match option {
            Opt::HostState => state = value.map(PathBuf::from),
            Opt::Tape => tape = value.map(PathBuf::from),
            Opt::Diagnose => diagnose = true,
            _ => return Err("unexpected option for host recording replay".into()),
        }
    }
    let tape = tape.ok_or("replay requires --tape TAPE")?.canonicalize()?;
    let header = crate::tape_cli::header(&tape)?;
    if header.kind != KIND_RUN {
        return Err("expected a Run recording".into());
    }
    let records = Records::read_only(&state.ok_or("replay requires --state DIR")?)?;
    let image = (|| -> io::Result<_> {
        let pin = records.pin(header.tape_id, unix_seconds()?)?;
        let assembly = records.assembly(pin)?;
        if launch_digest(&header.arg) != Ok(assembly.digest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording image differs from its pin",
            ));
        }
        records.open_image(assembly.digest)
    })();
    let image = match image {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("Unknown(ImageGone)");
            return Ok(3);
        }
        Err(error) => return Err(error.into()),
    };
    // Keep the verified file open across exec, even if collection expires the pin.
    let status = Command::new(format!("/proc/self/fd/{}", image.as_raw_fd()))
        .arg("--hirsel-replay-recording")
        .arg(if diagnose { "diagnose" } else { "same" })
        .arg(tape)
        .status()?;
    if status.success() {
        println!("Sealed");
    }
    Ok(status.code().unwrap_or(1))
}

fn launch_digest(snapshot: &[u8]) -> Result<ContentHash, Corrupt> {
    let mut cursor = Cursor::new(snapshot);
    for _ in 0..cursor.varint()? {
        cursor.bytes()?;
    }
    let digest = cursor.bytes()?.try_into().map_err(|_| Corrupt)?;
    Ok(ContentHash(digest))
}
