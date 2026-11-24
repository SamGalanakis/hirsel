use super::{file_kind, syscall};
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
    let st = file_stat(fd)?;
    let overflow = |_| io::Error::from_raw_os_error(libc::EOVERFLOW);
    let size = u64::try_from(st.st_size).map_err(overflow)?;
    let nanos = i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec);
    let nanos = i64::try_from(nanos).map_err(overflow)?;
    Ok(Outcome {
        value: size,
        written: nanos as u64,
        kind: file_kind(st.st_mode),
        ..Outcome::default()
    })
}
