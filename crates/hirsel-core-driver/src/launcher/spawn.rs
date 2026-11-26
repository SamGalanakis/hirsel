use super::ready::Readiness;
use super::session::{InstallKind, Launch, Mode, Selection, Status};
use super::{Conn, Image, ImageFile, above_launch_slots, cgroup, pipe, wait_pid};
use hirsel_core_ids::{hex, launch::LAUNCH_IMAGE_ENV};
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Seek, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

pub(super) use crate::host::launch::{ExecPoll, IMAGE_FD, PendingSpawn, Spawned, StartOutcome};

pub(super) fn move_envelope(command: &mut Command, envelope: &[u8]) {
    if !envelope.is_empty() {
        command.arg(format!(
            "--hirsel-move-envelope={}",
            hirsel_core_ids::hex::encode(envelope)
        ));
    }
}

pub(super) fn clear_activation(command: &mut Command) {
    command
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDNAMES")
        .env_remove("LISTEN_PIDFDID");
}

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

fn prepare(
    command: &Command,
    slots: &[(OwnedFd, RawFd)],
) -> io::Result<(Prepared, Vec<(RawFd, RawFd)>)> {
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
    Ok((Prepared { program, argv, env }, raw))
}

/// Begin starting `command` in `workdir`: returns `Pending` on Linux until the
/// exec error pipe closes, or `Started` synchronously on Darwin.
pub(super) fn begin(
    command: &Command,
    workdir: &Path,
    slots: &[(OwnedFd, RawFd)],
    cgroup: Option<&std::fs::File>,
    serving: bool,
) -> io::Result<StartOutcome> {
    let (prepared, raw) = prepare(command, slots)?;
    crate::host::launch::begin(&prepared, workdir, &raw, cgroup, serving)
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
    let (prepared, raw) = prepare(command, slots)?;
    crate::host::launch::start(&prepared, workdir, &raw, cgroup, serving)
}

pub(crate) fn pointers(words: &[CString]) -> Vec<*const libc::c_char> {
    words
        .iter()
        .map(|word| word.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect()
}

pub(super) struct RunningChild {
    pub(super) process: Spawned,
    pub(super) group: Option<cgroup::Group>,
    pub(super) status: Option<Status>,
}

impl RunningChild {
    pub(super) fn signal(&self, signal: i32) -> io::Result<()> {
        match (&self.group, signal, &self.status) {
            (Some(group), libc::SIGKILL, _) => group.kill(),
            (_, _, Some(_)) => Ok(()),
            _ => self.process.signal(signal),
        }
    }

    pub(super) fn wait(mut self) -> io::Result<Status> {
        self.reap_leader()?;
        if let Some(group) = &self.group {
            group.wait_empty()?;
        }
        self.finish()
    }

    pub(super) fn reap_leader(&mut self) -> io::Result<()> {
        if self.status.is_none() {
            self.status = Some(status_of(wait_pid(self.process.pid())?));
            if let Some(group) = &self.group {
                group.kill()?;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> io::Result<Status> {
        if let Some(group) = self.group {
            group.reap_orphans()?;
            group.remove()?;
        }
        Ok(self.status.expect("leader reaped"))
    }
}

fn status_of(status: ExitStatus) -> Status {
    match status.code() {
        Some(code) => Status::Code(code),
        None => Status::Signal(status.signal().unwrap_or(0)),
    }
}

pub(super) fn cleanup_cgroup(group: cgroup::Group) -> io::Result<()> {
    group.kill()?;
    group.wait_empty()?;
    group.reap_orphans()?;
    group.remove()
}

/// Prepared resources for a child whose exec acknowledgement is pending or just completed.
pub(super) struct PreparedChild {
    pub(super) selection: Option<Selection>,
    pub(super) endpoint: Option<Conn>,
    pub(super) report: Option<File>,
    pub(super) answer: Option<(File, u64)>,
    pub(super) install_target: Option<u64>,
    pub(super) triggers: Option<(PathBuf, u64)>,
    pub(super) cgroup: Option<cgroup::Group>,
}

/// Build the mode-specific command, fd slots, and optional cgroup, then start the child
/// via `begin` (nonblocking exec-ack on Linux, synchronous `posix_spawn` on Darwin).
pub(super) fn spawn_child(
    poll: &Readiness,
    cgroup_root: Option<&Path>,
    scratch: &Path,
    launch: &Launch,
    image: &Image,
    listener_fd: RawFd,
) -> io::Result<(StartOutcome, PreparedChild)> {
    let workdir = Path::new(&launch.workdir);
    let n_place = format!("--hirsel-n-place={}", image.n_place);
    let mut command = Command::new(&image.path);
    let mut slots: Vec<(OwnedFd, RawFd)> = Vec::new();
    let mut endpoint = None;
    let mut report = None;
    let mut answer = None;
    let mut install_target = None;
    let mut input = None;
    let mut triggers = None;
    let host_pid = std::process::id();
    move_envelope(&mut command, launch.mode.move_envelope());
    match &launch.mode {
        Mode::Serving(_, serve) => {
            command.arg("--hirsel-wake");
            if serve.idle != 0 {
                command.arg(format!("--hirsel-idle={}", serve.idle));
            }
            if let Some((hi, lo)) = serve.store_id {
                command.arg(format!("--hirsel-store-id={hi:016x}{lo:016x}"));
            }
            if let Some(version) = serve.witness {
                command.arg(format!("--hirsel-witness={version}"));
            }
            command
                .args(&image.prefix)
                .arg(&n_place)
                .arg(format!("--hirsel-activation={}", serve.activation))
                .args(&launch.args);
            std::fs::create_dir_all(scratch)?;
            command
                .env("HIRSEL_CONTROL_FD", "4")
                .env("HIRSEL_SCRATCH", scratch)
                .env("HIRSEL_HOST_PID", host_pid.to_string());
            if let Some(recording) = &launch.recording {
                command
                    .env("HIRSEL_RECORD", &recording.destination)
                    .env("HIRSEL_RECORD_ID", hex::encode(&recording.id));
            }
            let (ours, theirs) = UnixStream::pair()?;
            let (read, write) = pipe()?;
            slots.push((above_launch_slots(listener_fd)?, 3));
            slots.push((above_launch_slots(theirs.as_raw_fd())?, 4));
            slots.push((above_launch_slots(write.as_raw_fd())?, 5));
            endpoint = Some(Conn::new(poll, ours)?);
            report = Some(read);
        }
        Mode::Describe(_) | Mode::Preview(_, _) => {
            let flag = if let Mode::Preview(..) = launch.mode {
                "--hirsel-preview"
            } else {
                "--hirsel-describe"
            };
            command.args(&image.prefix).arg(&n_place).arg(flag);
            let out = tempfile::tempfile()?;
            answer = Some((out, image.n_place));
            if let Mode::Preview(_, old) = &launch.mode {
                let mut old_entries = tempfile::tempfile()?;
                old_entries.write_all(old)?;
                old_entries.rewind()?;
                input = Some(old_entries);
            }
        }
        Mode::Prepare(attempt, export, fates) => {
            if export.is_empty()
                || !export
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "export name"));
            }
            let path = scratch.join(export);
            command
                .args(&image.prefix)
                .arg(&n_place)
                .arg("--hirsel-prepare")
                .arg(&path)
                .args([
                    hex::encode(&attempt.bytes()),
                    hex::encode(&image.digest),
                    hex::encode(fates),
                ]);
            triggers = Some((path.join("TRIGGERS"), image.n_place));
        }
        Mode::Install(_, kind, _) => {
            let selection = match kind {
                InstallKind::Genesis => 0,
                InstallKind::Standalone(activation) => *activation,
            };
            command
                .args(&image.prefix)
                .arg(&n_place)
                .arg(format!("--hirsel-install-child={selection}"))
                .args(&launch.args)
                .env("HIRSEL_HOST_PID", host_pid.to_string());
            install_target = Some(selection.max(1));
            // HostReply::InstallRefused(WrongVersion) is the largest answer.
            answer = Some((tempfile::tempfile()?, 16));
        }
    }
    let null = File::options().read(true).write(true).open("/dev/null")?;
    let stdin = input.as_ref().unwrap_or(&null);
    let stdout = answer.as_ref().map_or(&null, |(out, _)| out);
    for (source, target) in [(stdin, 0), (stdout, 1), (&null, 2)] {
        slots.push((above_launch_slots(source.as_raw_fd())?, target));
    }
    let serving = matches!(launch.mode, Mode::Serving(..));
    if !serving {
        command
            .env_remove("HIRSEL_CONTROL_FD")
            .env_remove("HIRSEL_SCRATCH");
    }
    clear_activation(&mut command);
    if launch.recording.is_none() {
        command
            .env_remove("HIRSEL_RECORD")
            .env_remove("HIRSEL_RECORD_ID");
    }
    if serving {
        command.env("LISTEN_FDS", "1");
    }
    match &image.executable {
        ImageFile::Verified(file) => {
            let slot = IMAGE_FD;
            slots.push((above_launch_slots(file.as_raw_fd())?, slot));
            let parent = std::process::id();
            let digest = hex::encode(&image.digest);
            command.env(LAUNCH_IMAGE_ENV, format!("{parent}:{slot}:{digest}"));
        }
        #[cfg(test)]
        ImageFile::Script => {
            command.env_remove(LAUNCH_IMAGE_ENV);
        }
    }
    let group = cgroup_root
        .map(|root| cgroup::Group::create(root, &launch.child()))
        .transpose()?;
    let outcome = (|| {
        let directory = group.as_ref().map(cgroup::Group::directory).transpose()?;
        begin(&command, workdir, &slots, directory.as_ref(), serving)
    })();
    match outcome {
        Ok(outcome) => Ok((
            outcome,
            PreparedChild {
                selection: match &launch.mode {
                    Mode::Serving(_, serve) => {
                        Some(Selection::new(serve.activation, launch.assembly.clone()))
                    }
                    _ => None,
                },
                endpoint,
                report,
                answer,
                install_target,
                triggers,
                cgroup: group,
            },
        )),
        Err(error) => {
            if let Some(group) = group {
                cleanup_cgroup(group)?;
            }
            Err(error)
        }
    }
}
