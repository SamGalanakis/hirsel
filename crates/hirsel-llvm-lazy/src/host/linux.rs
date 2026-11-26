use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};

/// How libLLVM is opened: its symbols stay private to the handles that load it.
pub(crate) const LLVM_SCOPE: libc::c_int = libc::RTLD_LOCAL;

/// The sealed memory file the component was loaded from, kept open for the process.
pub(crate) struct Component(#[expect(dead_code, reason = "held only to keep the file open")] File);

/// A sealed memory file holding `bytes`, opened through its descriptor's path.
pub(crate) fn open_component(
    bytes: &[u8],
    open: impl FnOnce(&std::ffi::CStr) -> Result<*mut libc::c_void, String>,
) -> Result<(*mut libc::c_void, Component), String> {
    // SAFETY: memfd_create returns a new exclusively owned fd, not an existing
    // process handle. The sealed file is the linked component's exact bytes.
    let fd = unsafe {
        libc::memfd_create(
            c"hirsel-llvm".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: ownership of our newly created fd transfers exactly once.
    let mut image = unsafe { File::from_raw_fd(fd) };
    image.write_all(bytes).map_err(|e| e.to_string())?;
    // SAFETY: our own file has no writable mappings. Seal against replacement
    // while dlopen reads it; no external file or backend path is accepted.
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } < 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    let path = CString::new(format!("/proc/self/fd/{}", image.as_raw_fd())).unwrap();
    Ok((open(&path)?, Component(image)))
}

#[cfg(test)]
mod tests {
    /// The engine component loads from a memfd, so the process maps show
    /// when it arrives and that it arrives once.
    #[test]
    fn llvm_is_unmapped_until_first_object_and_loaded_once() {
        let maps = || std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps().contains("libLLVM"));
        let module = hirsel_core_ir::Module::default();
        crate::core::emit_ir(&module).unwrap();
        assert!(!maps().contains("libLLVM"));
        crate::core::emit_object(&module, crate::core::Optimization::None).unwrap();
        let loaded = maps();
        assert!(loaded.contains("libLLVM"));
        assert!(loaded.contains("memfd:hirsel-llvm"));
        crate::core::emit_object(&module, crate::core::Optimization::Speed).unwrap();
        let component_lines = |s: &str| {
            s.lines()
                .filter(|line| line.contains("memfd:hirsel-llvm"))
                .count()
        };
        assert_eq!(component_lines(&loaded), component_lines(&maps()));
    }
}
