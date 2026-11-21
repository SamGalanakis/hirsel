use crate::{abi::*, causes, os::OsProvider, process::*, provider::*};
use std::{fs::File, os::fd::OwnedFd};
struct Fixture {
    root: tempfile::TempDir,
    os: OsProvider,
    dir: u64,
    context: *mut crate::Context,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let out: OwnedFd = File::create(root.path().join("stdout")).unwrap().into();
        let err: OwnedFd = File::create(root.path().join("stderr")).unwrap().into();
        let mut os = OsProvider::new(out, err);
        let dir = os.grant(root.path()).unwrap();
        let context = hirsel_core_context_new(1 << 20);
        // SAFETY: fixture exclusively owns this fresh context.
        unsafe {
            assert!(!hirsel_core_task_start(context).pointer.is_null());
        }
        Self {
            root,
            os,
            dir,
            context,
        }
    }
    fn call(
        &mut self,
        op: u8,
        handle: u64,
        path: &[u8],
        mode: u8,
        bytes: &mut [u8],
        len: usize,
    ) -> (Outcome, usize) {
        let provider = self.os.provider();
        let mut request = Request {
            operation: op,
            mode,
            handle,
            path: path.as_ptr(),
            path_len: path.len(),
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len,
                capacity: bytes.len(),
            },
            site: 17,
        };
        // SAFETY: all operands are live and exclusively borrowed through delivery.
        let outcome = unsafe { hirsel_core_host(self.context, &provider, &mut request) };
        assert_eq!(outcome.status, 0);
        (outcome, request.buffer.len)
    }
    fn simple(&mut self, op: u8, handle: u64, path: &[u8], mode: u8) -> Outcome {
        self.call(op, handle, path, mode, &mut [], 0).0
    }
    fn open(&mut self, path: &[u8], mode: u8) -> u64 {
        let o = self.simple(OPEN, self.dir, path, mode);
        assert_eq!(o.error, IoError::OK);
        o.value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: no operation or transferred value remains live in this fixture.
        unsafe {
            hirsel_core_context_free(self.context);
        }
    }
}
#[test]
fn files_deliver_committed_prefixes_and_all_open_modes() {
    crate::openat2_required::require();
    let mut f = Fixture::new();
    assert_eq!(f.simple(OPEN, f.dir, b"absent", 0).error.tag, 1);
    let h = f.open(b"file", 3);
    assert_eq!(f.simple(OPEN, f.dir, b"file", 3).error.tag, 3);
    let mut bytes = *b"abcdef";
    assert_eq!(f.call(WRITE, h, b"", 0, &mut bytes, 6).0.written, 6);
    // Reading a write-only descriptor fails without touching the buffer/cursor.
    let mut read = [42; 4];
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 1).0.error.tag, 4);
    assert_eq!(read, [42; 4]);
    assert_eq!(f.simple(CLOSE, h, b"", 0).error, IoError::OK);
    let h = f.open(b"file", 0);
    let (out, len) = f.call(READ, h, b"", 0, &mut read, 1);
    assert_eq!((out.value, len, read), (3, 4, [42, b'a', b'b', b'c']));
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 4).0.value, 0);
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 0).0.value, 3);
    assert_eq!(&read[..3], b"def");
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 0).0.value, 0);
    assert_eq!(f.call(WRITE, h, b"", 0, &mut bytes, 6).0.error.tag, 4);
    f.simple(CLOSE, h, b"", 0);
    let h = f.open(b"file", 2);
    f.call(WRITE, h, b"", 0, &mut bytes, 1);
    f.simple(CLOSE, h, b"", 0);
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"abcdefa"
    );
    let h = f.open(b"file", 1);
    f.simple(CLOSE, h, b"", 0);
    assert!(
        std::fs::read(f.root.path().join("file"))
            .unwrap()
            .is_empty()
    );
    let h = f.open(b"new-append", 2);
    f.simple(CLOSE, h, b"", 0);
    for op in [STDOUT, STDERR] {
        assert_eq!(f.call(op, 0, b"", 0, &mut bytes, 6).0.error, IoError::OK);
    }
    for name in ["stdout", "stderr"] {
        assert_eq!(std::fs::read(f.root.path().join(name)).unwrap(), b"abcdef");
    }
}
/// A sync parks while the helper runs it and delivers the syscall's own
/// failure, never success: fdatasync(2) refuses a pipe with EINVAL.
#[test]
fn a_parked_sync_delivers_the_syscalls_failure() {
    let mut f = Fixture::new();
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two new descriptors into the array.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: both descriptors are new and owned here.
    let (read, write) = unsafe {
        use std::os::fd::FromRawFd;
        (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
    };
    drop(read);
    let h = f.os.adopt_file(write);
    let failed = f.simple(SYNC, h, b"", 0);
    assert_eq!(
        failed.error,
        IoError {
            tag: 5,
            code: libc::EINVAL as u32
        }
    );
}
#[test]
fn locks_are_exclusive_idempotent_and_end_at_close() {
    crate::openat2_required::require();
    let mut f = Fixture::new();
    let a = f.open(b"lock", 1);
    let b = f.open(b"lock", 1);
    for _ in 0..2 {
        assert_eq!(f.simple(TRY_LOCK, a, b"", 0).value, 1);
    }
    assert_eq!(f.simple(TRY_LOCK, b, b"", 0).value, 0);
    for _ in 0..2 {
        assert_eq!(f.simple(UNLOCK, a, b"", 0).error, IoError::OK);
    }
    assert_eq!(f.simple(TRY_LOCK, b, b"", 0).value, 1);
    f.simple(CLOSE, b, b"", 0);
    assert_eq!(f.simple(TRY_LOCK, a, b"", 0).value, 1);
    f.simple(CLOSE, a, b"", 0);
}

#[test]
fn paths_cannot_escape_grants_and_tree_drop_never_follows_links() {
    crate::openat2_required::require();
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    for p in [b"../escape".as_slice(), b"/tmp/escape"] {
        assert_eq!(f.simple(OPEN, f.dir, p, 1).error.tag, 2);
    }
    symlink("/tmp", f.root.path().join("outside")).unwrap();
    assert_eq!(f.simple(OPEN, f.dir, b"outside/escape", 1).error.tag, 2);
    let h = f.open(b"inside", 1);
    f.simple(CLOSE, h, b"", 0);
    symlink("inside", f.root.path().join("link")).unwrap();
    let h = f.open(b"link", 0);
    f.simple(CLOSE, h, b"", 0);
    let nested = f.root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let dir = f.simple(OPEN_DIR, f.dir, b"nested", 0);
    assert_eq!(dir.error, IoError::OK);
    let dir = dir.value;
    let inside_file = nested.join("original");
    let outside_file = f.root.path().join("elsewhere");
    std::fs::write(&inside_file, b"original").unwrap();
    std::fs::write(&outside_file, b"outside").unwrap();
    symlink("original", nested.join("name")).unwrap();
    let opened = f.simple(OPEN, dir, b"name", 0);
    assert_eq!(opened.error, IoError::OK);
    assert_eq!(f.simple(CLOSE, opened.value, b"", 0).error, IoError::OK);
    std::fs::remove_file(nested.join("name")).unwrap();
    symlink("../elsewhere", nested.join("name")).unwrap();
    assert_eq!(f.simple(OPEN, dir, b"name", 1).error.tag, 2);
    assert_eq!(f.simple(OPEN_DIR, dir, b"name", 0).error.tag, 2);
    // The final link is itself metadata inside the grant. Following it would
    // report the external file size or refuse the confined lookup.
    let link = f.simple(STAT, dir, b"name", 0);
    assert_eq!(
        (link.error, link.kind, link.value),
        (IoError::OK, 2, 12),
        "stat reports the link itself"
    );
    for path in [b"../elsewhere".as_slice(), b"/elsewhere"] {
        assert_eq!(f.simple(OPEN_DIR, dir, path, 0).error.tag, 2);
        assert_eq!(f.simple(STAT, dir, path, 0).error.tag, 2);
    }
    symlink("..", nested.join("up")).unwrap();
    assert_eq!(f.simple(OPEN_DIR, dir, b"up/nested", 0).error.tag, 2);
    assert_eq!(f.simple(STAT, dir, b"up/elsewhere", 0).error.tag, 2);
    std::fs::create_dir_all(nested.join("inside/child")).unwrap();
    symlink("inside", nested.join("through")).unwrap();
    assert_eq!(f.simple(OPEN_DIR, dir, b"through", 0).error.tag, 2);
    assert_eq!(
        f.simple(OPEN_DIR, dir, b"through/child", 0).error,
        IoError::OK
    );
    let listing = f.simple(ENTRIES, dir, b"", 0);
    assert_eq!(listing.error, IoError::OK);
    let mut records = [0; 258];
    let (batch, len) = f.call(NEXT_ENTRIES, listing.value, b"", 0, &mut records, 0);
    assert_eq!((batch.error, batch.value), (IoError::OK, 5));
    let mut names = Vec::new();
    let mut tail = &records[..len];
    while let Some(([kind, lo, hi], bytes)) = tail.split_first_chunk::<3>() {
        let (name, rest) = bytes.split_at(u16::from_le_bytes([*lo, *hi]) as usize);
        names.push((name, *kind));
        tail = rest;
    }
    assert!(tail.is_empty());
    names.sort_unstable();
    assert_eq!(
        names,
        [
            (b"inside".as_slice(), 1),
            (b"name", 2),
            (b"original", 0),
            (b"through", 2),
            (b"up", 2)
        ],
        "listing sees only direct names and does not dereference links"
    );
    assert_eq!(
        f.simple(CLOSE_ENTRIES, listing.value, b"", 0).error,
        IoError::OK
    );
    assert_eq!(std::fs::read(inside_file).unwrap(), b"original");
    assert_eq!(std::fs::read(outside_file).unwrap(), b"outside");
    assert_eq!(f.simple(MKDIR, f.dir, b"tree", 0).error, IoError::OK);
    assert_eq!(f.simple(MKDIR, f.dir, b"tree", 0).error.tag, 3);
    assert_eq!(f.simple(MKDIR, f.dir, b"tree/sub", 0).error, IoError::OK);
    std::fs::write(f.root.path().join("tree/sub/data"), b"data").unwrap();
    symlink("../../inside", f.root.path().join("tree/sub/link")).unwrap();
    assert_eq!(f.simple(REMOVE_TREE, f.dir, b"tree", 0).error, IoError::OK);
    assert!(!f.root.path().join("tree").exists());
    assert!(f.root.path().join("inside").exists());
    assert_eq!(f.simple(REMOVE, f.dir, b"link", 0).error, IoError::OK);
    assert_eq!(f.simple(REMOVE, f.dir, b"absent", 0).error.tag, 1);
    assert_eq!(f.simple(OPEN, f.dir, b"inside/child", 0).error.tag, 5);
    f.simple(MKDIR, f.dir, b"empty", 0);
    assert_eq!(f.simple(REMOVE, f.dir, b"empty", 0).error, IoError::OK);
    let mut name = [0; 40];
    name[..7].copy_from_slice(b"prefix:");
    let (out, n) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 7);
    assert_eq!(out.error, IoError::OK);
    assert_eq!(&name[..7], b"prefix:");
    assert!(
        f.root
            .path()
            .join(std::str::from_utf8(&name[..n]).unwrap())
            .is_dir()
    );
    assert_eq!(
        f.simple(REMOVE_TREE, f.dir, &name[..n], 0).error,
        IoError::OK
    );
}

#[test]
fn rename_rejects_empty_and_nul_components_before_any_effect() {
    let mut f = Fixture::new();
    std::fs::write(f.root.path().join("source"), b"source").unwrap();
    std::fs::write(f.root.path().join("victim"), b"victim").unwrap();
    for (from, to) in [
        (b"".as_slice(), b"dest".as_slice()),
        (b"source".as_slice(), b"".as_slice()),
        (b"bad\0name".as_slice(), b"dest".as_slice()),
        (b"source".as_slice(), b"bad\0name".as_slice()),
        // The suffix names an existing file, proving the first delimiter is
        // not caller-shiftable into a valid second path.
        (b"source".as_slice(), b"dest\0victim".as_slice()),
    ] {
        let mut joined = Vec::with_capacity(from.len() + to.len() + 1);
        joined.extend_from_slice(from);
        joined.push(0);
        joined.extend_from_slice(to);
        assert_eq!(f.simple(RENAME, f.dir, &joined, 0).error.tag, 2);
        assert_eq!(
            std::fs::read(f.root.path().join("source")).unwrap(),
            b"source"
        );
        assert_eq!(
            std::fs::read(f.root.path().join("victim")).unwrap(),
            b"victim"
        );
        assert!(!f.root.path().join("dest").exists());
        assert!(!f.root.path().join("bad").exists());
    }
}
/// Every entry of the fixture's root other than the standard streams: `f<bytes>`
/// for a regular file, `d` for a directory, `l<target>` for a symbolic link.
fn entries(root: &std::path::Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| n != "stdout" && n != "stderr")
        .map(|n| {
            let p = root.join(&n);
            let t = std::fs::symlink_metadata(&p).unwrap().file_type();
            let v = if t.is_symlink() {
                format!("l{}", std::fs::read_link(&p).unwrap().display())
            } else if t.is_dir() {
                "d".to_string()
            } else {
                format!(
                    "f{}",
                    String::from_utf8(std::fs::read(&p).unwrap()).unwrap()
                )
            };
            (n, v)
        })
        .collect();
    out.sort();
    out
}
/// One case per sentence of core.md's `host.rename` paragraph; the expected tree is
/// derived from the sentence, not from a run.
#[test]
fn rename_follows_each_sentence_of_core_md() {
    crate::openat2_required::require();
    /// Sentence, setup, `from`, `to`, the error tag, the tree after.
    type Case = (
        &'static str,
        fn(&std::path::Path),
        &'static str,
        &'static str,
        u8,
        &'static [(&'static str, &'static str)],
    );
    let cases: [Case; 9] = [
        (
            "replaces a regular file at `to`",
            |r| {
                std::fs::write(r.join("a"), "1").unwrap();
                std::fs::write(r.join("b"), "2").unwrap();
            },
            "a",
            "b",
            0,
            &[("b", "f1")],
        ),
        (
            "gives `from` the name `to`",
            |r| std::fs::write(r.join("a"), "1").unwrap(),
            "a",
            "c",
            0,
            &[("c", "f1")],
        ),
        (
            "a link at `from` is not followed",
            |r| {
                std::fs::write(r.join("a"), "1").unwrap();
                std::os::unix::fs::symlink("a", r.join("l")).unwrap();
            },
            "l",
            "c",
            2,
            &[("a", "f1"), ("l", "la")],
        ),
        (
            "a link at `to` is not followed",
            |r| {
                std::fs::write(r.join("a"), "1").unwrap();
                std::fs::write(r.join("t"), "2").unwrap();
                std::os::unix::fs::symlink("t", r.join("l")).unwrap();
            },
            "a",
            "l",
            2,
            &[("a", "f1"), ("l", "lt"), ("t", "f2")],
        ),
        (
            // The sentence is unconditional: NotFound even where `to` alone is Denied.
            "`from` absent is NotFound",
            |r| std::fs::create_dir(r.join("b")).unwrap(),
            "a",
            "b",
            1,
            &[("b", "d")],
        ),
        (
            "`from` a directory is Denied",
            |r| std::fs::create_dir(r.join("a")).unwrap(),
            "a",
            "c",
            2,
            &[("a", "d")],
        ),
        (
            "`to` a directory is Denied",
            |r| {
                std::fs::write(r.join("a"), "1").unwrap();
                std::fs::create_dir(r.join("b")).unwrap();
            },
            "a",
            "b",
            2,
            &[("a", "f1"), ("b", "d")],
        ),
        (
            "the same name succeeds and changes nothing",
            |r| std::fs::write(r.join("a"), "1").unwrap(),
            "a",
            "a",
            0,
            &[("a", "f1")],
        ),
        (
            "two names of the same file succeed and change nothing",
            |r| {
                std::fs::write(r.join("a"), "1").unwrap();
                std::fs::hard_link(r.join("a"), r.join("b")).unwrap();
            },
            "a",
            "b",
            0,
            &[("a", "f1"), ("b", "f1")],
        ),
    ];
    for (sentence, setup, from, to, tag, want) in cases {
        let mut f = Fixture::new();
        setup(f.root.path());
        let path = [from.as_bytes(), b"\0", to.as_bytes()].concat();
        let got = f.simple(RENAME, f.dir, &path, 0).error.tag;
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        assert_eq!((got, entries(f.root.path())), (tag, want), "{sentence}");
    }
}
#[test]
fn invalid_host_paths_are_denied_before_any_effect() {
    for operation in [OPEN, MKDIR, REMOVE, REMOVE_TREE, OPEN_DIR, STAT] {
        let mut f = Fixture::new();
        std::fs::create_dir(f.root.path().join("a")).unwrap();
        std::fs::write(f.root.path().join("keep"), b"root").unwrap();
        std::fs::write(f.root.path().join("a/keep"), b"child").unwrap();
        let entries = std::fs::read_dir(f.root.path()).unwrap().count();
        for path in [
            b"keep\0ignored".as_slice(),
            b".",
            b"..",
            b"/absolute",
            b"a/../..",
            b"a/../keep",
            b"a/./keep",
            b"a//keep",
            b"a/",
            b"",
        ] {
            assert_eq!(
                f.simple(operation, f.dir, path, 1).error,
                IoError::from_errno(libc::EACCES),
                "op={operation}, path={path:?}"
            );
            assert_eq!(std::fs::read(f.root.path().join("keep")).unwrap(), b"root");
            assert_eq!(
                std::fs::read(f.root.path().join("a/keep")).unwrap(),
                b"child"
            );
            assert_eq!(std::fs::read_dir(f.root.path()).unwrap().count(), entries);
            assert_eq!(
                std::fs::read_dir(f.root.path().join("a")).unwrap().count(),
                1
            );
        }
    }
}
#[test]
fn clocks_and_argv_are_borrowed_inputs() {
    let mut f = Fixture::new();
    let wall = f.simple(WALL_MS, 1, b"", 0).value;
    assert!(wall > 1_700_000_000_000);
    let mono = f.simple(MONO_NS, 1, b"", 0).value;
    assert!(f.simple(MONO_NS, 1, b"", 0).value >= mono);
    let bytes = b"argument";
    let args = [Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: 0,
    }];
    let mut stderr = Vec::new();
    let config = config(args.as_ptr(), 1, &mut stderr);
    // SAFETY: table and bytes live through every borrowed view.
    unsafe {
        (*f.context).run = &config;
        assert_eq!(hirsel_core_args_len(f.context, 1), 1);
        let v = hirsel_core_args_get(f.context, 1, 0);
        assert!(!v.is_null());
        assert_eq!((*v).capacity, 0);
        assert_eq!(std::slice::from_raw_parts((*v).data, (*v).len), bytes);
        assert!(hirsel_core_args_get(f.context, 1, u64::MAX).is_null());
    }
}
#[test]
fn temp_basename_matches_the_completed_carrier_and_invalid_prefixes_do_not_create() {
    crate::openat2_required::require();
    for prefix in [b"t-".as_slice(), b"", b"prefix--", b".", b".."] {
        let mut f = Fixture::new();
        let mut name = vec![0xa5; prefix.len() + 32];
        name[..prefix.len()].copy_from_slice(prefix);
        let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, prefix.len());
        assert_eq!(out.error, IoError::OK);
        assert!(len > prefix.len() && len <= name.len());
        assert_eq!(&name[..prefix.len()], prefix);
        assert!(
            name[prefix.len()..len]
                .iter()
                .all(u8::is_ascii_alphanumeric)
        );
        let basename = std::str::from_utf8(&name[..len]).unwrap();
        let file = f.simple(OPEN, out.value, b"x", 3);
        assert_eq!(file.error, IoError::OK);
        assert_eq!(
            f.call(WRITE, file.value, b"", 0, &mut [b'1'], 1).0.error,
            IoError::OK
        );
        assert_eq!(f.simple(CLOSE, file.value, b"", 0).error, IoError::OK);
        assert_eq!(
            std::fs::read(f.root.path().join(basename).join("x")).unwrap(),
            b"1"
        );
        assert_eq!(
            f.simple(REMOVE_TREE, f.dir, &name[..len], 0).error,
            IoError::OK
        );
        assert!(!f.root.path().join(basename).exists());
    }
    for prefix in [b"a/b".as_slice(), b"/", b"nul\0byte"] {
        let mut f = Fixture::new();
        let mut name = vec![0xa5; prefix.len() + 32];
        name[..prefix.len()].copy_from_slice(prefix);
        let before = name.clone();
        let before_entries = std::fs::read_dir(f.root.path()).unwrap().count();
        let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, prefix.len());
        assert_eq!(out.error, IoError::from_errno(libc::EACCES));
        assert_eq!((name, len), (before, prefix.len()));
        assert_eq!(
            std::fs::read_dir(f.root.path()).unwrap().count(),
            before_entries
        );
    }
}
#[test]
fn temp_collisions_retry_inside_issue_and_exhaustion_preserves_the_carrier() {
    crate::openat2_required::require();
    let mut f = Fixture::new();
    // Host fixture occupies the provider's deterministic first candidate. The
    // oracle is freshness/preservation, not which suffix the next attempt uses.
    let occupied = format!("retry-h{:x}x0", std::process::id());
    std::fs::create_dir(f.root.path().join(&occupied)).unwrap();
    let mut name = [0xa5; 40];
    name[..6].copy_from_slice(b"retry-");
    let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 6);
    assert_eq!(out.error, IoError::OK);
    assert_ne!(&name[..len], occupied.as_bytes());
    assert!(f.root.path().join(&occupied).is_dir());
    assert_eq!(
        f.simple(REMOVE_TREE, f.dir, &name[..len], 0).error,
        IoError::OK
    );

    let mut f = Fixture::new();
    for sequence in 0..crate::os::TEMP_ATTEMPTS {
        std::fs::create_dir(
            f.root
                .path()
                .join(format!("retry-h{:x}x{sequence:x}", std::process::id())),
        )
        .unwrap();
    }
    let mut name = [0xa5; 40];
    name[..6].copy_from_slice(b"retry-");
    let before = name;
    let before_entries = std::fs::read_dir(f.root.path()).unwrap().count();
    let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 6);
    assert_eq!(out.error, IoError::from_errno(libc::EAGAIN));
    assert_eq!((name, len), (before, 6));
    assert_eq!(
        std::fs::read_dir(f.root.path()).unwrap().count(),
        before_entries
    );
}
unsafe extern "C" fn diagnostic(state: *mut u8, p: *const u8, n: usize) {
    // SAFETY: test config owns a Vec sink and callback borrows exactly n bytes.
    unsafe {
        (&mut *state.cast::<Vec<u8>>()).extend_from_slice(std::slice::from_raw_parts(p, n));
    }
}
fn config(args: *const Buffer, args_len: usize, stderr: &mut Vec<u8>) -> Config {
    Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit: 1024,
        args,
        args_len,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: (stderr as *mut Vec<u8>).cast(),
        diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    }
}
unsafe extern "C" fn main_result(
    _: *mut crate::Context,
    config: *const Config,
    out: *mut MainResult,
) -> u8 {
    // SAFETY: test config encodes a case in grants_len; out is the live destination.
    unsafe {
        let n = (*config).grants_len;
        *out = MainResult {
            kind: if n == 256 {
                1
            } else if n == 257 {
                0
            } else {
                2
            },
            exit: n as u8,
        };
    }
    0
}
#[test]
fn process_reports_only_main_result_after_cleanup() {
    for n in 0..=257 {
        let mut stderr = Vec::new();
        let mut config = config(std::ptr::null(), 0, &mut stderr);
        config.grants_len = n;
        // SAFETY: generated-thunk contract is implemented by the fixture.
        let status = unsafe { hirsel_core_run(&config, main_result) };
        assert_eq!(
            status,
            if n == 257 {
                0
            } else if (1..=100).contains(&n) {
                n as u8
            } else {
                1
            }
        );
        assert!(stderr.is_empty());
    }
}
unsafe extern "C" fn failing_main(
    context: *mut crate::Context,
    config: *const Config,
    _: *mut MainResult,
) -> u8 {
    // SAFETY: prepaid task primary and resource cell, each recorded exactly once.
    unsafe {
        let resource = hirsel_core_resource_alloc(context, 8, 8, 0, 0).pointer;
        hirsel_core_raise(context, causes::Cause::fail(Default::default(), 41));
        hirsel_core_finalizer_begin(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_raise(context, causes::Cause::new(causes::OVERFLOW, 42));
        diagnostic((*config).state, b"cleaned\n".as_ptr(), 8);
        hirsel_core_finalizer_end(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_resource_free(std::ptr::NonNull::new(resource).unwrap(), 0);
    }
    1
}
#[test]
fn failure_chain_keeps_order_sites_and_cleanup_precedes_diagnostics() {
    let mut stderr = Vec::new();
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: live test config and thunk with explicit cleanup.
    assert_eq!(unsafe { hirsel_core_run(&config, failing_main) }, 101);
    assert_eq!(stderr, b"cleaned\nFail(\"\") at 41\nOverflow at 42\n");
}
#[test]
fn mkdtemp_bounds_and_pending_cancellation_happen_before_issue() {
    unsafe extern "C" fn forbidden(_: *mut u8, _: *mut Request) -> Outcome {
        panic!("operation must not issue")
    }
    unsafe extern "C" fn step_forbidden(s: *mut u8, r: *mut Request, _: u64) -> Step {
        Step {
            kind: STEP_DONE,
            outcome: forbidden(s, r),
            deadline_ns: 0,
        }
    }
    let provider = Provider {
        state: std::ptr::null_mut(),
        wait: crate::provider::immediate::wait,
        issue: step_forbidden,
        resume: crate::provider::immediate::resume,
        abort: crate::provider::immediate::abort,
        deliver,
    };
    for cancel in [false, true] {
        let f = Fixture::new();
        let mut bytes = [0xa5; 39];
        bytes[..8].copy_from_slice(b"prefix--");
        let before = bytes;
        let mut request = Request {
            operation: MKDTEMP,
            mode: 0,
            handle: f.dir,
            path: b"".as_ptr(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 39,
            },
            site: 88,
        };
        // SAFETY: live operands; fixture owns the context's pending request flag.
        unsafe {
            (*f.context)
                .cancel_requested
                .store(cancel, std::sync::atomic::Ordering::Release);
            assert_eq!(
                hirsel_core_host(f.context, &provider, &mut request).status,
                1
            );
            let chain = hirsel_core_causes_take(f.context);
            assert_eq!(
                (*hirsel_core_cause_value(chain)).kind(),
                if cancel {
                    causes::CANCELLED
                } else {
                    causes::BOUNDS
                }
            );
            hirsel_core_causes_drop(chain);
            assert_eq!(request.buffer.len, 8);
            assert_eq!(bytes, before);
        }
    }
}
#[test]
fn issued_operations_deliver_even_when_cancellation_arrives_at_delivery() {
    struct Script {
        context: *mut crate::Context,
        points: Vec<u8>,
        effect: Vec<u8>,
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
        // SAFETY: script exclusively owns state, request and the task during issue.
        unsafe {
            let state = &mut *state.cast::<Script>();
            state.points.push(0);
            state
                .effect
                .extend_from_slice(std::slice::from_raw_parts((*request).buffer.data, 2));
            (*state.context)
                .cancel_requested
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Outcome {
            written: 2,
            error: IoError {
                tag: 5,
                code: libc::ENOSPC as u32,
            },
            ..Outcome::default()
        }
    }
    unsafe extern "C" fn step_issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
        Step {
            kind: STEP_DONE,
            outcome: issue(s, r),
            deadline_ns: 0,
        }
    }
    unsafe extern "C" fn delivery(state: *mut u8, _: *mut Request, result: Outcome) -> Outcome {
        // SAFETY: delivery borrows the same script state after issue.
        unsafe {
            (&mut *state.cast::<Script>()).points.push(1);
        }
        result
    }
    let f = Fixture::new();
    let mut state = Script {
        context: f.context,
        points: vec![],
        effect: vec![],
    };
    let provider = Provider {
        state: (&mut state as *mut Script).cast(),
        wait: crate::provider::immediate::wait,
        issue: step_issue,
        resume: crate::provider::immediate::resume,
        abort: crate::provider::immediate::abort,
        deliver: delivery,
    };
    let mut bytes = *b"abcdef";
    let mut request = Request {
        operation: WRITE,
        mode: 0,
        handle: 0,
        path: b"".as_ptr(),
        path_len: 0,
        buffer: Buffer {
            data: bytes.as_mut_ptr(),
            len: 6,
            capacity: 6,
        },
        site: 19,
    };
    // SAFETY: script and all borrowed operands live through both provider points.
    unsafe {
        let out = hirsel_core_host(f.context, &provider, &mut request);
        assert_eq!((out.status, out.written, out.error.tag), (0, 2, 5));
        assert_eq!(state.points, [0, 1]);
        assert_eq!(state.effect, b"ab");
        assert_eq!(crate::tasks::hirsel_core_check_cancel(f.context, 20), 1);
    }
}
#[test]
fn scripted_host_errors_preserve_the_named_partial_effect_contracts() {
    struct Script {
        calls: usize,
        operation: u8,
        lock: bool,
        closed: bool,
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
        // SAFETY: test adapter gets exclusive script/request access at issue.
        unsafe {
            let state = &mut *state.cast::<Script>();
            assert_eq!((*request).operation, state.operation);
            state.calls += 1;
            if state.operation == CLOSE {
                state.closed = true;
                state.lock = false;
            }
            Outcome {
                error: IoError::from_errno(libc::EIO),
                ..Outcome::default()
            }
        }
    }
    unsafe extern "C" fn step_issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
        Step {
            kind: STEP_DONE,
            outcome: issue(s, r),
            deadline_ns: 0,
        }
    }
    for operation in [
        OPEN,
        READ,
        WRITE,
        CLOSE,
        TRY_LOCK,
        UNLOCK,
        MKDIR,
        MKDTEMP,
        REMOVE,
        REMOVE_TREE,
        STDOUT,
        STDERR,
    ] {
        let f = Fixture::new();
        let mut state = Script {
            calls: 0,
            operation,
            lock: operation == UNLOCK || operation == CLOSE,
            closed: false,
        };
        let mut bytes = [42; 40];
        let before = bytes;
        let mut request = Request {
            operation,
            mode: 0,
            handle: 0,
            path: b"x".as_ptr(),
            path_len: 1,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 3,
                capacity: 40,
            },
            site: 1,
        };
        let provider = Provider {
            state: (&mut state as *mut Script).cast(),
            wait: crate::provider::immediate::wait,
            issue: step_issue,
            resume: crate::provider::immediate::resume,
            abort: crate::provider::immediate::abort,
            deliver,
        };
        // SAFETY: live script and caller capacity for both provider phases.
        let out = unsafe { hirsel_core_host(f.context, &provider, &mut request) };
        assert_eq!(
            (out.status, out.error, state.calls),
            (0, IoError::from_errno(libc::EIO), 1)
        );
        assert_eq!((request.buffer.len, bytes), (3, before));
        assert_eq!(state.closed, operation == CLOSE);
        assert_eq!(state.lock, operation == UNLOCK);
    }
}
#[test]
fn guarded_root_routes_real_os_calls_through_the_separate_host_stack() {
    crate::openat2_required::require();
    unsafe extern "C" fn main(
        context: *mut crate::Context,
        config: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        // SAFETY: live run grants/provider, static path, no returned owners.
        unsafe {
            *result = MainResult::default();
            let config = &*config;
            let mut request = Request {
                operation: MKDIR,
                mode: 0,
                handle: *config.grants,
                path: b"from-main".as_ptr(),
                path_len: 9,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 1,
            };
            assert_eq!(
                hirsel_core_host(context, config.provider, &mut request).error,
                IoError::OK
            );
            request.operation = REMOVE;
            assert_eq!(
                hirsel_core_host(context, config.provider, &mut request).error,
                IoError::OK
            );
        }
        0
    }
    use std::ptr;
    let mut fixture = Fixture::new();
    let provider = fixture.os.provider();
    let grants = [fixture.dir];
    let mut stderr = vec![];
    let mut config = config(ptr::null(), 0, &mut stderr);
    config.provider = &provider;
    config.grants = grants.as_ptr();
    config.grants_len = 1;
    // SAFETY: config, OS adapter and grants outlive the guarded root.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 0);
    assert!(stderr.is_empty());
    assert!(!fixture.root.path().join("from-main").exists());
}
#[test]
fn foreign_host_call_canonicalizes_nan_and_restores_floats() {
    use crate::foreign::{RESULT_F32, RESULT_F64, hirsel_core_foreign_stack, invoke};

    unsafe extern "C" fn poison(args: *mut u64) -> u64 {
        let here = 0u8;
        assert!(crate::fiber::room(1 << 20));
        // SAFETY: the fixture lends one pinned argument word.
        unsafe { args.write(std::hint::black_box(&here as *const u8).addr() as u64) };
        #[cfg(target_arch = "x86_64")]
        {
            let mxcsr: u32 = 0xdfc0;
            let x87: u16 = 0x0f7f;
            // SAFETY: both words are readable and use supported control bits.
            unsafe {
                core::arch::asm!(
                    "ldmxcsr [{mxcsr}]",
                    "fldcw [{x87}]",
                    mxcsr = in(reg) &mxcsr,
                    x87 = in(reg) &x87,
                    options(nostack, preserves_flags, readonly),
                );
            }
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: FPCR is writable in userspace. These bits change rounding and flushing.
        unsafe {
            core::arch::asm!("msr fpcr, {fpcr}", fpcr = in(reg) 0x0180_0000u64);
        }
        0xfff0_789a_ff80_1234
    }

    unsafe extern "C" fn main(
        _: *mut crate::Context,
        _: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        let mut word = 0;
        // SAFETY: the fixture pins its argument and result. The thunk is bounded
        // and returns without retaining the argument or switching control.
        unsafe {
            *result = MainResult::default();
            assert_eq!(
                hirsel_core_foreign_stack(poison, &mut word, RESULT_F64),
                0x7ff8_0000_0000_0000
            );
            assert!((&raw const word).addr().abs_diff(word as usize) >= 1 << 20);
        }
        0
    }

    let mut stderr = vec![];
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: the fixture pins its configuration through the root task.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 0);
    assert!(stderr.is_empty());

    // Drive the helper-side call form on a native thread. Reading its control
    // words after return detects restoration even without a stack switch.
    let mut word = 0;
    // SAFETY: this test thread has a native stack and lends the thunk one word.
    assert_eq!(
        unsafe { invoke(poison, &mut word, RESULT_F32) },
        0x7fc0_0000
    );
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr = 0u32;
        let mut x87 = 0u16;
        // SAFETY: both destinations are writable and fit their control word.
        unsafe {
            core::arch::asm!(
                "stmxcsr [{mxcsr}]",
                "fnstcw [{x87}]",
                mxcsr = in(reg) &raw mut mxcsr,
                x87 = in(reg) &raw mut x87,
                options(nostack, preserves_flags),
            );
        }
        assert_eq!((mxcsr, x87), (0x1f80, 0x037f));
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: FPCR is readable in userspace.
        unsafe { core::arch::asm!("mrs {fpcr}, fpcr", fpcr = out(reg) fpcr) };
        assert_eq!(fpcr, 0);
    }
}

#[test]
fn foreign_helper_parks_the_issuer_and_delivers_its_writes() {
    use crate::{foreign::hirsel_core_foreign_wait, tasks::*};
    use std::sync::{Condvar, Mutex};

    type Gate = (Mutex<bool>, Condvar);

    unsafe extern "C" fn open(_: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the input holds the parent-owned gate pointer. Its only
        // mutation is under the mutex, and the parent joins before dropping it.
        let gate = unsafe { &**input.cast::<*const Gate>() };
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        0
    }

    unsafe extern "C" fn mark(args: *mut u64) -> u64 {
        // SAFETY: the call lends three initialized argument words. The
        // borrowed gate remains pinned and only its mutex-protected bit changes.
        unsafe {
            assert_ne!(args.read(), libc::pthread_self());
            let gate = &*std::ptr::with_exposed_provenance::<Gate>(args.add(1).read() as usize);
            drop(
                gate.1
                    .wait_while(gate.0.lock().unwrap(), |open| !*open)
                    .unwrap(),
            );
            args.add(2).write(0xabab_abab);
        }
        4
    }

    unsafe extern "C" fn main(
        context: *mut crate::Context,
        _: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        let gate = Gate::default();
        let mut gate_pointer = &raw const gate;
        let mut count = 0;
        let mut unit = 0u8;
        let input = ValueLayout {
            bytes: 8,
            align: 8,
            logical: 8,
            clean: None,
        };
        let output = ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            clean: None,
        };
        // SAFETY: both borrowed slots and the gate outlive the helper and the
        // child. The task exclusively owns its result slot and child handle.
        unsafe {
            *result = MainResult::default();
            let child = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut gate_pointer).cast(),
                open,
                TaskLayout {
                    input,
                    result: output,
                },
                1,
            );
            assert!(!child.task.is_null());
            let mut words = [
                libc::pthread_self(),
                gate_pointer.expose_provenance() as u64,
                0,
            ];
            assert_eq!(
                hirsel_core_foreign_wait(context, mark, words.as_mut_ptr(), 0, &mut count, 2),
                0
            );
            assert_eq!((count, words[2]), (4, 0xabab_abab));
            let joined = hirsel_core_join(context, child.task, &mut unit, 3);
            assert_eq!((joined.kind, joined.status), (0, 0));
        }
        0
    }

    let mut stderr = Vec::new();
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: config and stderr stay live through the run and helper shutdown.
    assert_eq!(
        unsafe { hirsel_core_run_with_foreign_helpers(&config, main) },
        0
    );
    assert!(stderr.is_empty());
}

#[test]
fn literal_failure_messages_keep_static_backing_and_render_one_line_with_their_site() {
    unsafe extern "C" fn main(
        context: *mut crate::Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let message = b"static\nmessage";
        // SAFETY: static String carrier has capacity zero and stays live forever.
        unsafe {
            hirsel_core_raise_string(
                context,
                17,
                message.as_ptr().cast_mut(),
                message.len() as u64,
                0,
            );
        }
        1
    }
    let mut stderr = vec![];
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: generated failure consumes only its prepaid primary and static carrier.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 101);
    assert_eq!(stderr, b"Fail(\"static\\nmessage\") at 17\n");
}

#[path = "rule_provider_tests.rs"]
mod rule_provider_tests;
