use std::ffi::{CStr, CString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;

/// Darwin loads images by path only and has no sealed memory file. The component is
/// written read-only into a fresh 0700 directory that only this user can enter, loaded
/// from there, and both are removed once the loader has mapped it. The image keeps the
/// linker's signature, so the kernel checks every page it maps against it.
pub(crate) fn open_component(
    bytes: &[u8],
    open: impl FnOnce(&CStr) -> Result<*mut libc::c_void, String>,
) -> Result<(*mut libc::c_void, Component), String> {
    let error = |cause: io::Error| format!("LLVM component: {cause}");
    let root = std::env::temp_dir();
    let mut template = root.join("hirsel-llvm-XXXXXX").into_os_string().into_vec();
    template.push(0);
    // SAFETY: the template is writable and NUL-terminated; mkdtemp creates a new
    // 0700 directory and rewrites the template in place with its name.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(error(io::Error::last_os_error()));
    }
    template.pop();
    let directory = std::path::PathBuf::from(std::ffi::OsString::from_vec(template));
    let image = directory.join("engine.dylib");
    let loaded = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o500)
            .open(&image)?;
        file.write_all(bytes)
    })()
    .map_err(error)
    .and_then(|()| open(&CString::new(image.as_os_str().as_bytes()).unwrap()));
    let _ = fs::remove_file(&image);
    let _ = fs::remove_dir(&directory);
    Ok((loaded?, Component))
}

/// Nothing outlives the load: the mapped image needs no file.
pub(crate) struct Component;
