//! The component is part of the executable, rather than an independently
//! replaceable plugin. Its bytes are mapped only when LLVM emits an object.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/component.rs"));
#[repr(C)]
struct Output {
    data: *mut u8,
    len: usize,
    failed: u8,
}
type Compile = unsafe extern "C" fn(*const u8, usize, u8, *mut Output);
type Free = unsafe extern "C" fn(*mut u8);
#[repr(C)]
struct Input {
    data: *const u8,
    len: usize,
    imported: u8,
}
type Program =
    unsafe extern "C" fn(*const Input, usize, *const *const libc::c_char, usize, u8, *mut Output);
struct Backend {
    compile: Compile,
    program: Program,
    free: Free,
    _image: File,
}
fn dl_error() -> String {
    // SAFETY: dlerror's thread-local message is borrowed until the next dl call.
    unsafe {
        let error = libc::dlerror();
        if error.is_null() {
            "dynamic loader failed".into()
        } else {
            CStr::from_ptr(error).to_string_lossy().into_owned()
        }
    }
}
fn open(name: &CStr) -> Result<*mut libc::c_void, String> {
    // SAFETY: the name is terminated. Handles remain loaded for process life,
    // like rustc's codegen backend, so no callback can outlive its code.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        Err(dl_error())
    } else {
        Ok(handle)
    }
}
fn load() -> Result<Backend, String> {
    let llvm = CString::new(LLVM_LIBRARY).unwrap();
    open(&llvm)?;
    // SAFETY: memfd_create returns a new exclusively owned fd, not an existing
    // process handle. The sealed file is the linked component's exact bytes.
    let fd = unsafe {
        libc::memfd_create(
            c"hirsel-llvm".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: ownership of our newly created fd transfers exactly once.
    let mut image = unsafe { File::from_raw_fd(fd) };
    image.write_all(COMPONENT).map_err(|e| e.to_string())?;
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
        return Err(std::io::Error::last_os_error().to_string());
    }
    let path = CString::new(format!("/proc/self/fd/{}", image.as_raw_fd())).unwrap();
    let handle = open(&path)?;
    // SAFETY: these two exports and C layouts are defined by the embedded
    // engine.c built in this same artifact. No foreign component is selected.
    let compile = unsafe { libc::dlsym(handle, c"hirsel_llvm_compile_v1".as_ptr()) };
    let free = unsafe { libc::dlsym(handle, c"hirsel_llvm_output_free_v1".as_ptr()) };
    let program = unsafe { libc::dlsym(handle, c"hirsel_llvm_program_v1".as_ptr()) };
    if compile.is_null() || free.is_null() || program.is_null() {
        return Err(dl_error());
    }
    Ok(Backend {
        // SAFETY: same-build functions have the signatures above. They own no
        // Rust values across the C boundary; output is copied then freed there.
        compile: unsafe { std::mem::transmute::<*mut libc::c_void, Compile>(compile) },
        // SAFETY: this export is built with the Input/Output C layouts above.
        program: unsafe { std::mem::transmute::<*mut libc::c_void, Program>(program) },
        free: unsafe { std::mem::transmute::<*mut libc::c_void, Free>(free) },
        _image: image,
    })
}
fn backend() -> Result<&'static Backend, String> {
    static BACKEND: OnceLock<Result<Backend, String>> = OnceLock::new();
    BACKEND.get_or_init(load).as_ref().map_err(Clone::clone)
}
pub(crate) fn compile(ir: &str, speed: bool) -> Result<Vec<u8>, String> {
    let backend = backend()?;
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: IR stays borrowed through the call; engine.c returns an owned
    // malloc buffer on every exit, which its paired free consumes once.
    unsafe {
        (backend.compile)(ir.as_ptr(), ir.len(), u8::from(speed), &mut output);
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

pub(crate) fn program(units: &[crate::Unit<'_>], exports: &[String]) -> Result<Vec<u8>, String> {
    let backend = backend()?;
    let inputs: Vec<_> = units
        .iter()
        .map(|u| Input {
            data: u.ir.as_ptr(),
            len: u.ir.len(),
            imported: u8::from(u.imported),
        })
        .collect();
    let names: Vec<_> = exports
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap())
        .collect();
    let pointers: Vec<_> = names.iter().map(|s| s.as_ptr()).collect();
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: inputs and terminated export names stay borrowed through the
    // synchronous C call. The paired component allocator owns its output.
    unsafe {
        (backend.program)(
            inputs.as_ptr(),
            inputs.len(),
            pointers.as_ptr(),
            pointers.len(),
            1,
            &mut output,
        );
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}
