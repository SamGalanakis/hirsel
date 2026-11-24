use super::syscall;
use crate::provider::Outcome;
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd},
};

pub(super) fn zero_extension(fd: BorrowedFd<'_>) -> io::Result<bool> {
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the directory owns fd, and fstatfs initializes the output on success.
    syscall(unsafe { libc::fstatfs(fd.as_raw_fd(), fs.as_mut_ptr()) } as isize)?;
    // SAFETY: fstatfs succeeded.
    match unsafe { fs.assume_init() }.f_type {
        0x0102_1994 => return Ok(true), // tmpfs
        0xEF53 => (),                   // ext4
        _ => return Ok(false),
    }
    let dev = file_stat(fd)?.st_dev;
    let major = libc::major(dev);
    let minor = libc::minor(dev);
    let Ok(partitions) = std::fs::read_to_string("/proc/partitions") else {
        return Ok(false);
    };
    let name = partitions.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let row_major = fields.next()?.parse::<libc::c_uint>().ok()?;
        let row_minor = fields.next()?.parse::<libc::c_uint>().ok()?;
        fields.next()?; // block count
        let name = fields.next()?;
        ((row_major, row_minor) == (major, minor)).then_some(name)
    });
    Ok(name
        .and_then(|name| std::fs::read_to_string(format!("/proc/fs/ext4/{name}/options")).ok())
        .is_some_and(|options| {
            options
                .lines()
                .any(|line| matches!(line, "data=ordered" | "data=journal"))
        }))
}

fn file_stat(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the caller retains fd and fstat writes the supplied stat.
    syscall(unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) } as isize)?;
    // SAFETY: fstat succeeded.
    Ok(unsafe { st.assume_init() })
}

pub(super) fn stat(fd: BorrowedFd<'_>) -> io::Result<Outcome> {
    if crate::units::uses_file_ring() {
        let mut st = std::mem::MaybeUninit::<libc::statx>::zeroed();
        let entry = io_uring::opcode::Statx::new(
            io_uring::types::Fd(fd.as_raw_fd()),
            c"".as_ptr(),
            st.as_mut_ptr().cast(),
        )
        .flags(libc::AT_EMPTY_PATH)
        .mask(libc::STATX_TYPE | libc::STATX_SIZE | libc::STATX_MTIME)
        .build();
        // SAFETY: the caller owns fd and statx storage lives through the CQE.
        unsafe { crate::units::file_request(entry) }?;
        // SAFETY: zero-initialized statx is valid and the successful CQE has
        // published the kernel's result.
        let st = unsafe { st.assume_init() };
        return stat_outcome(
            st.stx_size,
            i128::from(st.stx_mtime.tv_sec),
            i128::from(st.stx_mtime.tv_nsec),
            u32::from(st.stx_mode),
        );
    }
    let st = file_stat(fd)?;
    let size =
        u64::try_from(st.st_size).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    stat_outcome(
        size,
        i128::from(st.st_mtime),
        i128::from(st.st_mtime_nsec),
        st.st_mode,
    )
}

fn stat_outcome(size: u64, sec: i128, nsec: i128, mode: libc::mode_t) -> io::Result<Outcome> {
    let nanos = i64::try_from(sec * 1_000_000_000 + nsec)
        .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    Ok(Outcome {
        value: size,
        written: nanos as u64,
        kind: file_kind(mode),
        ..Outcome::default()
    })
}

pub(super) fn file_kind(mode: libc::mode_t) -> u8 {
    match mode & libc::S_IFMT {
        libc::S_IFREG => 0,
        libc::S_IFDIR => 1,
        libc::S_IFLNK => 2,
        _ => 3,
    }
}
