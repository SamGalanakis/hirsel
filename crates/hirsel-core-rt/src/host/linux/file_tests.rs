use super::*;
use crate::{
    provider::{CLOSE, DIRECT_ALIGNMENT, IoError, MODE_DIRECT, WRITE, WRITE_AT, ZERO_AT},
    provider_tests::Fixture,
};
use std::{
    fs,
    os::fd::{AsFd, AsRawFd},
    os::unix::fs::OpenOptionsExt,
    process::Command,
};

#[test]
fn file_issuer_writes_and_reaps() {
    const CHILD: &str = "HIRSEL_FILE_ISSUER_LAW";
    match std::env::var(CHILD).as_deref() {
        Ok("pool") => {
            let config = unsafe { std::mem::zeroed() };
            assert_eq!(
                crate::units::launch(1, 1, &config, &|_| {
                    write_law();
                    0
                }),
                0
            );
        }
        Ok("tied" | "epoll") => {
            assert!(!crate::units::untied());
            write_law();
        }
        _ => {
            let thread = std::thread::current();
            let name = thread.name().unwrap();
            for child in ["tied", "pool", "epoll"] {
                let mut cmd = Command::new(std::env::current_exe().unwrap());
                cmd.args(["--exact", name]).env(CHILD, child);
                if child == "epoll" {
                    cmd.env(hirsel_core_net::REACTOR_ENV, "epoll");
                }
                let output = cmd.output().unwrap();
                assert!(
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                    "{child}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

fn write_law() {
    let ring = hirsel_core_net::reactor().unwrap().completes_files();
    if std::env::var_os("HIRSEL_TEST_REQUIRE_FILE_RING").is_some() {
        assert_eq!(
            ring,
            std::env::var("HIRSEL_FILE_ISSUER_LAW").unwrap() != "epoll"
        );
    }
    let mut f = Fixture::new();
    let file = f.os.adopt_file(
        fs::File::create(f.root.path().join("journal"))
            .unwrap()
            .into(),
    );
    let mut bytes = *b"abcdefgh";
    assert_eq!(f.call(WRITE, file, &[], 4, &mut bytes, 8).0.written, 8);
    assert_eq!(
        f.call(WRITE_AT, file, &2u64.to_le_bytes(), 4, &mut bytes[1..4], 3)
            .0
            .written,
        3
    );
    let mut range = [0u8; 16];
    range[..8].copy_from_slice(&5u64.to_le_bytes());
    range[8..].copy_from_slice(&2u64.to_le_bytes());
    assert_eq!(f.simple(ZERO_AT, file, &range, 4).written, 2);
    assert_eq!(
        fs::read(f.root.path().join("journal")).unwrap(),
        b"abbcd\0\0h"
    );
    assert_eq!(RING_WRITES.get(), if ring { 2 } else { 0 });
    assert_eq!(
        SYSCALL_WRITES.load(Ordering::Relaxed),
        if ring { 0 } else { 2 }
    );
    if ring {
        fixed_writes(&mut f, file);
        batched_slots();
    }
    assert_eq!(f.simple(CLOSE, file, &[], 0).error, IoError::OK);
}

fn fixed_writes(f: &mut Fixture, file: u64) {
    const LEN: usize = DIRECT_ALIGNMENT as usize;
    let mut pair = hirsel_core_net::file_ring::FileBuffers::new(LEN).unwrap();
    register_file_buffers(&pair).unwrap();
    pair.buffer(0).fill(0x2a);
    pair.buffer(1).fill(0xbc);
    let segment = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_DSYNC)
        .open(f.root.path().join("segment"))
        .unwrap();
    crate::host::sync::enable_direct(segment.as_fd(), libc::O_WRONLY | libc::O_DSYNC).unwrap();
    let direct = f.os.adopt_file(segment.into());
    let fd = f.os.file_descriptor(direct);
    // Verify the durable direct handle on the host that supplies this proof.
    if std::env::var_os("HIRSEL_TEST_REQUIRE_FILE_RING").is_some() {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_eq!(
            flags & (libc::O_DIRECT | libc::O_DSYNC),
            libc::O_DIRECT | libc::O_DSYNC
        );
    }
    assert_eq!(
        f.call(
            WRITE_AT,
            direct,
            &DIRECT_ALIGNMENT.to_le_bytes(),
            MODE_DIRECT,
            pair.buffer(1),
            LEN
        )
        .0
        .written,
        LEN as u64
    );
    assert_eq!(
        f.call(
            WRITE_AT,
            file,
            &1u64.to_le_bytes(),
            4,
            &mut pair.buffer(0)[3..6],
            3
        )
        .0
        .written,
        3
    );
    assert_eq!(FIXED_WRITES.get(), 2);
    assert_eq!(
        fs::read(f.root.path().join("journal")).unwrap(),
        b"a***d\0\0h"
    );
    let segment = fs::read(f.root.path().join("segment")).unwrap();
    assert_eq!(segment.len(), 2 * LEN);
    assert!(segment[..LEN].iter().all(|&v| v == 0));
    assert!(segment[LEN..].iter().all(|&v| v == 0xbc));
    let ro = fs::File::open(f.root.path().join("journal")).unwrap();
    assert_eq!(
        write_part(
            ro.as_raw_fd(),
            pair.buffer(1),
            Some(0),
            true,
            DIRECT_ALIGNMENT
        )
        .unwrap_err()
        .raw_os_error(),
        Some(libc::EBADF)
    );
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    assert_eq!(
        write_part(
            full.as_raw_fd(),
            pair.buffer(0),
            Some(0),
            true,
            DIRECT_ALIGNMENT
        )
        .unwrap_err()
        .raw_os_error(),
        Some(libc::ENOSPC)
    );
    let invalid = f
        .call(
            WRITE_AT,
            direct,
            &0u64.to_le_bytes(),
            MODE_DIRECT,
            &mut pair.buffer(1)[1..],
            LEN - 1,
        )
        .0;
    assert_eq!(
        (invalid.written, invalid.error),
        (0, IoError::from_errno(libc::EINVAL))
    );
    let mut plain = *b"!?";
    assert_eq!(
        f.call(WRITE_AT, file, &0u64.to_le_bytes(), 4, &mut plain[1..], 1)
            .0
            .written,
        1
    );
    assert_eq!(
        (FIXED_WRITES.get(), SYSCALL_WRITES.load(Ordering::Relaxed)),
        (2, 0)
    );
    assert_eq!(f.simple(CLOSE, direct, &[], 0).error, IoError::OK);
}

fn batched_slots() {
    const N: usize = 17;
    let file = tempfile::tempfile().unwrap();
    let bytes: [u8; N] = std::array::from_fn(|i| i as u8 + 1);
    let answers = [const { Cell::new(None::<i32>) }; N];
    unsafe fn finish(ctx: *const (), n: i32) {
        // SAFETY: the issuer retains the corresponding cell through drain.
        assert_eq!(
            unsafe { (*ctx.cast::<Cell<Option<i32>>>()).replace(Some(n)) },
            None
        );
    }
    let requests: [_; N] = std::array::from_fn(|i| {
        let entry = io_uring::opcode::Write::new(
            io_uring::types::Fd(file.as_raw_fd()),
            bytes[i..].as_ptr(),
            1,
        )
        .offset(i as u64)
        .build();
        FileRequest::new(entry, (&raw const answers[i]).cast(), finish)
    });
    let issuer = FileRing::new(2).unwrap();
    for request in &requests {
        // SAFETY: every source, fd, request and cell outlives issuer teardown.
        unsafe { issuer.queue(request) };
    }
    drop(issuer);
    assert!(answers.iter().all(|answer| answer.get() == Some(1)));
    use std::os::unix::fs::FileExt;
    let mut got = [0; N];
    file.read_exact_at(&mut got, 0).unwrap();
    assert_eq!(got, bytes);
}

#[test]
fn device_reader_reports_support_and_reads_unaligned_eof() {
    use crate::provider::{
        DEVICE_READ_EXTENT, DEVICE_READ_UNAVAILABLE, MODE_READ_DEVICE, OPEN, READ_AT,
    };
    use std::os::fd::FromRawFd;

    crate::openat2_required::require();
    let mut f = Fixture::new();
    let dir = f.dir;
    assert!(!crate::provider::needs_writable_dir(OPEN, MODE_READ_DEVICE));
    let absent = f.simple(OPEN, dir, b"missing", MODE_READ_DEVICE);
    assert_eq!(absent.error, IoError::from_errno(libc::ENOENT));
    // A memfd is shmem even on kernels where F_SETFL(O_DIRECT) succeeds.
    let raw = unsafe { libc::memfd_create(c"device-read".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw >= 0);
    let ram = unsafe { fs::File::from_raw_fd(raw) };
    assert!(!crate::host::sync::enable_device_read(ram.as_fd()).unwrap());

    const LEN: usize = DEVICE_READ_EXTENT as usize;
    let mut data = vec![0x5a; LEN + 3];
    data[LEN..].copy_from_slice(b"end");
    let path = f.root.path().join("pack");
    fs::write(&path, &data).unwrap();
    let opened = f.simple(OPEN, dir, b"pack", MODE_READ_DEVICE);
    let fs_type = {
        let file = fs::File::open(path).unwrap();
        let mut stat = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        unsafe { stat.assume_init() }.f_type
    };
    if fs_type == libc::TMPFS_MAGIC {
        assert_eq!(opened.error, IoError::from_errno(DEVICE_READ_UNAVAILABLE));
        return;
    }
    assert_eq!(opened.error, IoError::OK);
    let fd = f.os.file_descriptor(opened.value);
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_DIRECT,
        0
    );
    #[repr(align(65536))]
    struct Extent([u8; LEN]);
    let mut buf = Extent([0; LEN]);
    let bad = f.call(
        READ_AT,
        opened.value,
        &1u64.to_le_bytes(),
        MODE_READ_DEVICE,
        &mut buf.0,
        0,
    );
    assert_eq!((bad.0.error, bad.1), (IoError::from_errno(libc::EINVAL), 0));
    let bad = f.call(
        READ_AT,
        opened.value,
        &0u64.to_le_bytes(),
        MODE_READ_DEVICE,
        &mut buf.0,
        1,
    );
    assert_eq!((bad.0.error, bad.1), (IoError::from_errno(libc::EINVAL), 1));
    for (at, n) in [(0u64, LEN), (LEN as u64, 3), (2 * LEN as u64, 0)] {
        let (out, filled) = f.call(
            READ_AT,
            opened.value,
            &at.to_le_bytes(),
            MODE_READ_DEVICE,
            &mut buf.0,
            0,
        );
        assert_eq!((out.error, filled), (IoError::OK, n));
        if n > 0 {
            assert_eq!(&buf.0[..n], &data[at as usize..at as usize + n]);
        }
    }
    assert_eq!(
        f.simple(CLOSE, opened.value, b"", MODE_READ_DEVICE).error,
        IoError::OK
    );
}
