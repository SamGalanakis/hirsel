use super::*;
use std::{cell::Cell, fs::File, os::fd::AsRawFd};
struct Fixture {
    root: tempfile::TempDir,
    sink: File,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let sink = File::create(root.path().join("stderr")).unwrap();
        Self { root, sink }
    }
    fn stderr(&self) -> Vec<u8> {
        std::fs::read(self.root.path().join("stderr")).unwrap()
    }
    fn run(
        &self,
        kinds: &[u8],
        directories: usize,
        root_limit: u64,
        probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
        entry: crate::process::Entry,
    ) -> u8 {
        use std::os::unix::ffi::OsStrExt;
        let root = self.root.path().as_os_str().as_bytes();
        let dirs = vec![root; directories];
        self.run_in(kinds, &dirs, std::ptr::null(), root_limit, probe, entry)
    }
    /// A run granted `directories`, opened by `directory_ops` (none: never opened).
    fn run_in(
        &self,
        kinds: &[u8],
        directories: &[&[u8]],
        directory_ops: *const DirectoryOperations,
        root_limit: u64,
        probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
        entry: crate::process::Entry,
    ) -> u8 {
        use std::os::unix::ffi::OsStrExt;
        let dirs: Vec<_> = directories
            .iter()
            .map(|path| View {
                data: path.as_ptr(),
                len: path.len(),
            })
            .collect();
        let marker = self.root.path().join("main-ran");
        let marker = marker.as_os_str().as_bytes();
        let args = [View {
            data: marker.as_ptr(),
            len: marker.len(),
        }];
        let names = [b"stream".as_slice(), b"scratch", b"backup"];
        let mut ordinal = [0u32; 11];
        let requirements: Vec<_> = kinds
            .iter()
            .map(|&kind| {
                let r = Requirement {
                    kind,
                    index: ordinal[kind as usize] as usize,
                };
                ordinal[kind as usize] += 1;
                r
            })
            .collect();
        let names: Vec<_> = (0..kinds.len())
            .map(|index| {
                let value = if kinds.len() == 1 {
                    b"scratch".as_slice()
                } else {
                    names[index]
                };
                View {
                    data: value.as_ptr(),
                    len: value.len(),
                }
            })
            .collect();
        let input = OsConfig {
            directory_ops,
            process_ops: std::ptr::null(),
            socket_activation: std::ptr::null(),
            secret_grants: core::ptr::null(),
            secret_grants_len: 0,
            env_patterns: crate::process::View {
                data: std::ptr::null(),
                len: 0,
            },
            secret_ops: std::ptr::null(),
            env_ops: std::ptr::null(),
            exec_bindings: std::ptr::null(),
            exec_bindings_len: 0,
            exec_ops: std::ptr::null(),
            target: crate::process::View {
                data: std::ptr::null(),
                len: 0,
            },
            root_limit,
            args: args.as_ptr(),
            args_len: args.len(),
            directories: dirs.as_ptr(),
            directories_len: dirs.len(),
            grant_mask: 1 << STDOUT,
            stdout_fd: self.sink.as_raw_fd(),
            stderr_fd: self.sink.as_raw_fd(),
            parameters: requirements.as_ptr(),
            parameters_len: requirements.len(),
            net_rules: std::ptr::null(),
            net_rules_len: 0,
        };
        // SAFETY: every input/name view and diagnostic fd stays live through this run.
        unsafe { run_with_probe(&input, names.as_ptr(), entry, probe) }
    }
}
unsafe extern "C" fn entered(
    _: *mut crate::Context,
    config: *const Config,
    _: *mut crate::process::MainResult,
) -> u8 {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: the fixture passed one copied argv String naming its private marker.
    let arg = unsafe { &*(*config).args };
    let path = unsafe { std::slice::from_raw_parts(arg.data, arg.len) };
    std::fs::write(Path::new(OsStr::from_bytes(path)), b"main").unwrap();
    0
}
#[test]
fn unsupported_openat2_names_first_dir_and_refuses_before_main_without_effects() {
    let fixture = Fixture::new();
    let probes = Cell::new(0);
    let status = fixture.run(
        &[STDOUT, DIR, DIR],
        2,
        4096,
        |_, _| {
            probes.set(probes.get() + 1);
            Err(io::Error::from_raw_os_error(libc::ENOSYS))
        },
        entered,
    );
    assert_eq!((status, probes.get()), (103, 1));
    assert_eq!(
        fixture.stderr(),
        b"not started: Dir requires openat2 (Linux 5.6 or later, permitted by the sandbox)\n"
    );
    assert!(!fixture.root.path().join("main-ran").exists());
    assert_eq!(std::fs::read_dir(fixture.root.path()).unwrap().count(), 1);
}
#[test]
fn multiple_dirs_probe_once_before_main() {
    let fixture = Fixture::new();
    let probes = Cell::new(0);
    let status = fixture.run(
        &[STDOUT, DIR, DIR],
        2,
        4096,
        |_, _| {
            probes.set(probes.get() + 1);
            Ok(())
        },
        entered,
    );
    assert_eq!((status, probes.get()), (0, 1));
    assert_eq!(
        std::fs::read(fixture.root.path().join("main-ran")).unwrap(),
        b"main"
    );
    assert!(fixture.stderr().is_empty());
}
#[test]
fn no_dir_runs_and_never_probes_even_with_unused_configured_directories() {
    let fixture = Fixture::new();
    let status = fixture.run(
        &[],
        2,
        4096,
        |_, _| panic!("host has no openat2: must never probe"),
        entered,
    );
    assert_eq!(status, 0);
    assert!(fixture.root.path().join("main-ran").exists());
    assert!(fixture.stderr().is_empty());
}
#[test]
fn missing_grant_is_a_named_not_started_refusal() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            &[DIR],
            0,
            4096,
            |_, _| panic!("missing Dir cannot probe"),
            entered
        ),
        103
    );
    assert_eq!(
        fixture.stderr(),
        b"not started: grant `scratch` is not in the run's configuration\n"
    );
    assert!(!fixture.root.path().join("main-ran").exists());
}
/// Bug: a directory grant that could not be opened was refused with the bare
/// OS error, naming no path. The refusal names the grant as configured and,
/// for a relative one, the path it resolves to against the run's working
/// directory (PROG-4: a test's relative directory grant).
#[test]
fn unopenable_directory_grant_names_its_path_and_resolution() {
    let fixture = Fixture::new();
    let missing = b"npb-1621-no-such-directory";
    let status = fixture.run_in(
        &[DIR],
        &[missing],
        &hirsel_core_os_directory_ops,
        4096,
        |_, _| panic!("an unopened Dir cannot probe"),
        entered,
    );
    assert_eq!(status, 103);
    let resolved = std::env::current_dir()
        .unwrap()
        .join("npb-1621-no-such-directory");
    assert_eq!(
        String::from_utf8(fixture.stderr()).unwrap(),
        format!(
            "not started: directory grant `npb-1621-no-such-directory` ({}): No such file or directory (os error 2)\n",
            resolved.display()
        )
    );
    assert!(!fixture.root.path().join("main-ran").exists());
}
#[test]
fn insufficient_root_refuses_with_the_shared_startup_charge_before_main() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(&[], 0, 0, |_, _| panic!("no Dir"), entered),
        103
    );
    let requested =
        hirsel_core_layout::charge::TASK_RECORD + hirsel_core_layout::charge::CAUSE_CELL;
    assert_eq!(
        fixture.stderr(),
        format!(
            "not started: the root limit is below main's startup charge of {requested} bytes\n"
        )
        .as_bytes()
    );
    assert!(!fixture.root.path().join("main-ran").exists());
}
#[test]
fn exit_cannot_select_the_not_started_status() {
    unsafe extern "C" fn entry(
        _: *mut crate::Context,
        _: *const Config,
        out: *mut crate::process::MainResult,
    ) -> u8 {
        // SAFETY: native entry exclusively initializes its result slot.
        unsafe {
            *out = crate::process::MainResult {
                kind: 2,
                exit: 103,
                ..Default::default()
            }
        };
        0
    }
    let fixture = Fixture::new();
    assert_eq!(fixture.run(&[], 0, 4096, |_, _| panic!("no Dir"), entry), 1);
    assert!(fixture.stderr().is_empty());
}

#[test]
fn exec_startup_uses_all_three_uids_and_both_capability_words() {
    use crate::exec::unprivileged;
    assert!(unprivileged([1000; 3], [0; 2], [0; 2]));
    for index in 0..3 {
        let mut uids = [1000; 3];
        uids[index] = 0;
        assert!(!unprivileged(uids, [0; 2], [0; 2]));
    }
    for index in 0..2 {
        let mut caps = [0; 2];
        caps[index] = 1;
        assert!(!unprivileged([1000; 3], [0; 2], caps));
        assert!(!unprivileged([1000; 3], caps, [0; 2]));
    }
}
