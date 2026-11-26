//! Starting a launcher child on Darwin: `posix_spawn`, never `fork`
//! (launch.md, "macOS"). The spawn closes every descriptor that no slot names,
//! starts a new session, defaults every signal and clears the mask; the
//! working directory is an `fchdir` action. Darwin executes by path only, so
//! a verified image is executed from a private clone of exactly that file,
//! which the child also receives as its image slot.
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::launcher::spawn::{Prepared, pointers};

pub(crate) const IMAGE_FD: RawFd = 6;

const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;
/// Exit watches share the launcher's token space with descriptors, which are
/// below 2^31; a child's exit is keyed above them by its PID.
const EXIT_TOKEN: u64 = 1 << 40;

/// A started child, pinned by its PID until it is reaped: the launcher is its
/// parent and reaps it only through `wait_pid`.
pub(crate) struct Spawned {
    pid: libc::pid_t,
    /// The private directory holding the clone the child executes. It lives
    /// as long as the child, whose image check reads the running image's path.
    _image: Option<PrivateImage>,
}

impl Spawned {
    pub(crate) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Send `signal` to the child; a kill reaches its whole process group,
    /// which its new session made. A child that has exited is not an error.
    pub(crate) fn signal(&self, signal: i32) -> io::Result<()> {
        let target = if signal == libc::SIGKILL {
            -self.pid
        } else {
            self.pid
        };
        // SAFETY: the child is unreaped, so its PID and group are still ours.
        if unsafe { libc::kill(target, signal) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Whether the child has exited, without waiting or reaping it.
    pub(crate) fn exited(&self) -> bool {
        // SAFETY: an all-zero siginfo is a valid output record.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid writes one record and WNOWAIT leaves the child unreaped.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        rc == 0 && info.si_pid == self.pid
    }

    /// The child's exit source for a readiness set: `EVFILT_PROC` on its PID,
    /// a one-shot registration nothing removes.
    pub(crate) fn exit_source(&self) -> (hirsel_core_net::ready::Child<'_>, u64, Option<RawFd>) {
        (
            hirsel_core_net::ready::Child::new(self.pid),
            EXIT_TOKEN | self.pid as u64,
            None,
        )
    }
}

/// A 0700 directory holding one executable clone, removed with it.
struct PrivateImage {
    directory: PathBuf,
    path: PathBuf,
}

impl PrivateImage {
    /// Clone `image` (an APFS clone where the volume allows, else a copy) into a new
    /// private directory, and open the clone for the child's image slot.
    fn of(image: RawFd) -> io::Result<(Self, File)> {
        let mut template = std::env::temp_dir()
            .join("hirsel-image-XXXXXX")
            .into_os_string()
            .into_vec();
        template.push(0);
        // SAFETY: the template is NUL-terminated and writable.
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        let directory = PathBuf::from(OsString::from_vec(template));
        let private = Self {
            path: directory.join("image"),
            directory,
        };
        let parent = File::open(&private.directory)?;
        // SAFETY: both descriptors and the name are live for the call.
        let cloned =
            unsafe { libc::fclonefileat(image, parent.as_raw_fd(), c"image".as_ptr(), 0) } == 0;
        if !cloned {
            // SAFETY: dup returns a fresh descriptor or fails.
            let duplicate = unsafe { libc::dup(image) };
            if duplicate < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the duplicate is owned here and read from its start.
            let mut source = unsafe { File::from_raw_fd(duplicate) };
            std::io::Seek::rewind(&mut source)?;
            let mut copy = File::create_new(&private.path)?;
            io::copy(&mut source, &mut copy)?;
        }
        std::fs::set_permissions(
            &private.path,
            std::os::unix::fs::PermissionsExt::from_mode(0o500),
        )?;
        let file = File::open(&private.path)?;
        Ok((private, file))
    }
}

impl Drop for PrivateImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

pub(crate) fn start(
    prepared: &Prepared,
    workdir: &Path,
    raw: &[(RawFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<Spawned> {
    if cgroup.is_some() {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut slots = raw.to_vec();
    let mut program = prepared.program.clone();
    let mut image = None;
    let mut image_file = None;
    if let Some(slot) = slots.iter_mut().find(|(_, target)| *target == IMAGE_FD) {
        let (private, file) = PrivateImage::of(slot.0)?;
        program = CString::new(private.path.as_os_str().as_bytes())?;
        slot.0 = file.as_raw_fd();
        image = Some(private);
        image_file = Some(file);
    }
    let argv = pointers(&prepared.argv);
    // The child's PID does not exist until the spawn returns, so a serving
    // child's activation names its parent, the launcher (net.md, "Inherited listener").
    let activation = serving
        .then(|| CString::new(format!("LISTEN_PID={}", std::process::id())))
        .transpose()?;
    let mut envp = pointers(&prepared.env);
    if let Some(activation) = &activation {
        envp.insert(envp.len() - 1, activation.as_ptr());
    }
    let cwd = crate::package_files::open_directory(workdir)?;
    let mut actions = Actions::new()?;
    for &(source, target) in &slots {
        // SAFETY: the actions object is initialized; the descriptor is live.
        spawned(unsafe { libc::posix_spawn_file_actions_adddup2(&mut actions.0, source, target) })?;
    }
    // SAFETY: as above; the directory stays open through the spawn.
    spawned(unsafe { libc::posix_spawn_file_actions_addfchdir(&mut actions.0, cwd.as_raw_fd()) })?;
    let attributes = Attributes::new()?;
    let mut pid: libc::pid_t = 0;
    // SAFETY: the program, argv and envp are NUL-terminated and pinned through
    // the call; the actions and attributes are initialized.
    spawned(unsafe {
        libc::posix_spawn(
            &mut pid,
            program.as_ptr(),
            &actions.0,
            &attributes.0,
            argv.as_ptr().cast(),
            envp.as_ptr().cast(),
        )
    })?;
    drop(image_file);
    Ok(Spawned { pid, _image: image })
}

/// `posix_spawn*` calls return their errno rather than setting it.
fn spawned(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

struct Actions(libc::posix_spawn_file_actions_t);

impl Actions {
    fn new() -> io::Result<Self> {
        let mut actions = std::ptr::null_mut();
        // SAFETY: init allocates the opaque actions object.
        spawned(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
        Ok(Self(actions))
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        // SAFETY: initialized in `new` and destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// Close every descriptor no action names, start a new session, default every
/// signal and clear the mask.
struct Attributes(libc::posix_spawnattr_t);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut attributes = std::ptr::null_mut();
        // SAFETY: init allocates the opaque attributes object.
        spawned(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
        let mut this = Self(attributes);
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | POSIX_SPAWN_SETSID
            | libc::POSIX_SPAWN_SETSIGDEF
            | libc::POSIX_SPAWN_SETSIGMASK;
        // SAFETY: the sets are initialized before the attributes copy them.
        unsafe {
            let mut all: libc::sigset_t = std::mem::zeroed();
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigfillset(&mut all);
            libc::sigemptyset(&mut none);
            spawned(libc::posix_spawnattr_setflags(
                &mut this.0,
                flags as libc::c_short,
            ))?;
            spawned(libc::posix_spawnattr_setsigdefault(&mut this.0, &all))?;
            spawned(libc::posix_spawnattr_setsigmask(&mut this.0, &none))?;
        }
        Ok(this)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized in `new` and destroyed once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}
