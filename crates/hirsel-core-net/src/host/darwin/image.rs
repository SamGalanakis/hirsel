//! Executable format check on Darwin: a native image is a 64-bit Mach-O
//! executable, alone or as a slice of a universal file, which names no program
//! interpreter.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

/// The verified executable's real path on Darwin (`F_GETPATH` avoids linking
/// `realpath$DARWIN_EXTSN`).
pub(crate) fn real_path(file: &File, _profile: &str) -> io::Result<CString> {
    let mut buffer = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes, NUL included.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&buffer).map_err(|_| io::ErrorKind::InvalidData)?;
    Ok(path.to_owned())
}

const CPU_TYPE_ARM64: u32 = 0x0100_000c;

/// Whether a 64-bit Mach-O executable for this machine starts at `offset`.
fn executable(file: &File, offset: u64) -> bool {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_EXECUTE: u32 = 2;
    let mut header = [0; 16];
    if file.read_exact_at(&mut header, offset).is_err() {
        return false;
    }
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    word(0) == MH_MAGIC_64 && word(4) == CPU_TYPE_ARM64 && word(12) == MH_EXECUTE
}

/// A native executable is a 64-bit Mach-O executable for this machine, or a
/// universal file with one for this machine among its slices; dyld is implicit.
/// The kernel chooses the slice it runs among those for this machine, so each of
/// them must be an executable that lies inside the file.
pub(crate) fn interpreter(file: &mut File) -> io::Result<Option<Arc<CStr>>> {
    let invalid = || io::Error::other("expected a native Mach-O executable without a #! line");
    const FAT_MAGIC: u32 = 0xcafe_babe;
    // A slice entry is five big-endian words: CPU type, subtype, offset, size, alignment.
    const ENTRY: usize = 20;
    // The kernel reads the slice table from the file's first page.
    const TABLE: usize = 4096;
    // A 64-bit Mach-O header.
    const HEADER: u64 = 32;
    let mut start = [0; 8];
    file.read_exact_at(&mut start, 0).map_err(|_| invalid())?;
    let big = |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().unwrap());
    if big(&start[..4]) != FAT_MAGIC {
        return executable(file, 0).then_some(None).ok_or_else(invalid);
    }
    let entries = big(&start[4..]) as usize * ENTRY;
    if start.len() + entries > TABLE {
        return Err(invalid());
    }
    let mut table = vec![0; entries];
    file.read_exact_at(&mut table, start.len() as u64)
        .map_err(|_| invalid())?;
    let length = file.metadata()?.len();
    let mut native = 0;
    for entry in table.chunks_exact(ENTRY) {
        if big(&entry[..4]) != CPU_TYPE_ARM64 {
            continue;
        }
        let (offset, size) = (
            u64::from(big(&entry[8..12])),
            u64::from(big(&entry[12..16])),
        );
        if size < HEADER || offset + size > length || !executable(file, offset) {
            return Err(invalid());
        }
        native += 1;
    }
    if native == 0 {
        return Err(invalid());
    }
    Ok(None)
}
