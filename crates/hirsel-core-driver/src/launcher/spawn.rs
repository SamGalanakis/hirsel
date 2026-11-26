use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::Command;

pub(super) use crate::host::launch::{IMAGE_FD, Spawned};

/// What a child is started with: its absolute program path, its arguments
/// (the program first) and its environment, all as C strings. The host
/// provider adds the activation's `LISTEN_PID` for a serving child.
pub(crate) struct Prepared {
    pub(crate) program: CString,
    pub(crate) argv: Vec<CString>,
    pub(crate) env: Vec<CString>,
}

fn cstring(word: &OsStr) -> io::Result<CString> {
    CString::new(word.as_encoded_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// Start `command` in `workdir` in a new session, with each slot's descriptor
/// at its target and nothing else open, and pin it until it is reaped.
pub(super) fn start(
    command: &Command,
    workdir: &Path,
    slots: &[(OwnedFd, RawFd)],
    cgroup: Option<&std::fs::File>,
    serving: bool,
) -> io::Result<Spawned> {
    let image = std::path::absolute(command.get_program())?;
    let program = cstring(image.as_os_str())?;
    let argv: Vec<_> = std::iter::once(Ok(program.clone()))
        .chain(command.get_args().map(cstring))
        .collect::<io::Result<_>>()?;
    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(key.to_owned(), value.to_owned());
        } else {
            environment.remove(key);
        }
    }
    let env: Vec<_> = environment
        .into_iter()
        .map(|(mut key, value)| {
            key.push("=");
            key.push(value);
            cstring(&key)
        })
        .collect::<io::Result<_>>()?;
    let raw: Vec<_> = slots
        .iter()
        .map(|(fd, slot)| (fd.as_raw_fd(), *slot))
        .collect();
    let prepared = Prepared { program, argv, env };
    crate::host::launch::start(&prepared, workdir, &raw, cgroup, serving)
}

pub(crate) fn pointers(words: &[CString]) -> Vec<*const libc::c_char> {
    words
        .iter()
        .map(|word| word.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect()
}
