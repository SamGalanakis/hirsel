//! The component is part of the executable, rather than an independently
//! replaceable plugin. Its bytes are mapped only when LLVM emits an object.
//! libLLVM is not part of the executable: [`llvm_library`] names the one file this
//! process loads, so an LLVM product's key can name that file's content.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/component.rs"));
const LLVM_LIBRARY: &str = "libLLVM.so.23.1";
const BUCK_ENGINE: &str = "../../../../../../tools/buck2/toolchains/llvm-files/engine";
#[repr(C)]
struct Output {
    data: *mut u8,
    len: usize,
    failed: u8,
}
type Compile = unsafe extern "C" fn(
    *const u8,
    usize,
    u8,
    *const libc::c_char,
    *const libc::c_char,
    *mut Output,
);
type Free = unsafe extern "C" fn(*mut u8);
#[repr(C)]
struct Input {
    data: *const u8,
    len: usize,
    module: *mut libc::c_void,
}
type Reuse = unsafe extern "C" fn(
    *mut libc::c_void,
    *const u8,
    usize,
    *const u8,
    usize,
    *const u8,
    usize,
) -> i32;
type Deliver = unsafe extern "C" fn(*mut libc::c_void, *const u8, usize);
type Program = unsafe extern "C" fn(
    *const Input,
    usize,
    *const *const libc::c_char,
    usize,
    u8,
    *const libc::c_char,
    *const libc::c_char,
    u32,
    Reuse,
    Deliver,
    *mut libc::c_void,
    *mut Output,
);
pub(crate) enum ProgramUnit<'a> {
    Native(crate::construction::NativeModule),
    Text(&'a str),
}
struct Backend {
    compile: Compile,
    program: Program,
    free: Free,
    construction: &'static crate::construction::Api,
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
/// The libLLVM file this process's LLVM backend loads, resolved once and
/// without loading it: a request served from the store keys on it and never
/// maps LLVM. The loader opens exactly this path, so the file an LLVM key
/// names is the file that emits its bytes.
///
/// Only the shipped relative path or the pinned build-tree engine can supply
/// LLVM. The library used by clang has the same SONAME, so a name-only open
/// cannot distinguish it from the compiler's engine.
pub fn llvm_library() -> Result<&'static Path, String> {
    static LIBRARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    LIBRARY
        .get_or_init(resolve_engine)
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

fn resolve_engine() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let origin = executable
        .parent()
        .ok_or("compiler has no installation directory")?;
    let bundled = origin.join("lib").join(LLVM_LIBRARY);
    let prefix = origin.join("../lib").join(LLVM_LIBRARY);
    let buck = origin.join(BUCK_ENGINE).join(LLVM_LIBRARY);
    let (path, expected) = if bundled.exists() {
        (bundled, LLVM_BUILD_ID.into())
    } else if origin.file_name().is_some_and(|name| name == "bin") && prefix.exists() {
        (prefix, LLVM_BUILD_ID.into())
    } else if buck.is_file() {
        let expected = std::env::var("HIRSEL_LLVM_EXPERIMENT_BUILD_ID")
            .unwrap_or_else(|_| LLVM_BUILD_ID.into());
        (buck, expected)
    } else if let Some((_, cargo)) = CARGO_ENGINE.filter(|(profile, _)| {
        let profile = Path::new(profile);
        origin == profile || origin == profile.join("deps")
    }) {
        (PathBuf::from(cargo), LLVM_BUILD_ID.into())
    } else {
        return Err(format!(
            "pinned LLVM engine missing beside {}",
            executable.display()
        ));
    };
    let path = path
        .canonicalize()
        .map_err(|error| format!("LLVM engine {}: {error}", path.display()))?;
    check_engine(&path, &expected)?;
    Ok(path)
}

fn check_engine(path: &Path, expected: &str) -> Result<(), String> {
    use object::Object as _;
    use std::fmt::Write as _;

    let error =
        |detail: &dyn std::fmt::Display| format!("LLVM engine {}: {detail}", path.display());
    let source = File::open(path).map_err(|cause| error(&cause))?;
    let cache = object::read::ReadCache::new(source);
    let elf = object::File::parse(&cache).map_err(|cause| error(&cause))?;
    let id = elf
        .build_id()
        .map_err(|cause| error(&cause))?
        .ok_or_else(|| error(&"no build ID"))?;
    let mut actual = String::with_capacity(id.len() * 2);
    for byte in id {
        write!(actual, "{byte:02x}").unwrap();
    }
    if actual != expected {
        return Err(error(&format_args!(
            "wrong build ID {actual}; expected {expected}"
        )));
    }
    Ok(())
}
fn load() -> Result<Backend, String> {
    let llvm = CString::new(llvm_library()?.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
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
    let program = unsafe { libc::dlsym(handle, c"hirsel_llvm_program_v6".as_ptr()) };
    // SAFETY: the table getter is part of the same embedded component ABI.
    let construction = unsafe { libc::dlsym(handle, c"hirsel_llvm_builder_v1".as_ptr()) };
    if compile.is_null() || free.is_null() || program.is_null() || construction.is_null() {
        return Err(dl_error());
    }
    Ok(Backend {
        // SAFETY: same-build functions have the signatures above. They own no
        // Rust values across the C boundary; output is copied then freed there.
        compile: unsafe { std::mem::transmute::<*mut libc::c_void, Compile>(compile) },
        // SAFETY: this export is built with the Input/Output C layouts above.
        program: unsafe { std::mem::transmute::<*mut libc::c_void, Program>(program) },
        free: unsafe { std::mem::transmute::<*mut libc::c_void, Free>(free) },
        // SAFETY: the table is static, its types match construction.rs, and the
        // component stays mapped for the process lifetime.
        construction: unsafe {
            let getter = std::mem::transmute::<
                *mut libc::c_void,
                unsafe extern "C" fn() -> *const crate::construction::Api,
            >(construction);
            &*getter()
        },
        _image: image,
    })
}
fn backend() -> Result<&'static Backend, String> {
    static BACKEND: OnceLock<Result<Backend, String>> = OnceLock::new();
    BACKEND.get_or_init(load).as_ref().map_err(Clone::clone)
}
pub(crate) fn construction_api() -> Result<&'static crate::construction::Api, String> {
    Ok(backend()?.construction)
}
pub(crate) unsafe fn free_construction_output(data: *mut u8) {
    // SAFETY: callers pass exactly one buffer returned by this loaded component.
    unsafe { (backend().expect("construction already loaded").free)(data) }
}
pub(crate) fn compile(
    ir: &str,
    speed: bool,
    target: hirsel_core_layout::Target,
) -> Result<Vec<u8>, String> {
    let triple = CString::new(target.triple()).unwrap();
    let cpu = CString::new(target.cpu()).unwrap();
    let backend = backend()?;
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: IR stays borrowed through the call; engine.c returns an owned
    // malloc buffer on every exit, which its paired free consumes once.
    unsafe {
        (backend.compile)(
            ir.as_ptr(),
            ir.len(),
            u8::from(speed),
            triple.as_ptr(),
            cpu.as_ptr(),
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

/// Compile one natively constructed module standalone, keeping its order.
pub(crate) fn compile_module(
    module: crate::construction::NativeModule,
    speed: bool,
    target: hirsel_core_layout::Target,
) -> Result<Vec<u8>, String> {
    let mut program = emit(
        vec![ProgramUnit::Native(module)],
        &[],
        speed,
        target,
        0,
        &mut |_, _, _| false,
    )?;
    Ok(program
        .objects
        .pop()
        .expect("a standalone module is one object"))
}

/// A program's parts, in order, and the objects generated for the parts
/// `reuse` did not answer true for: one per part on `jobs` threads, or with
/// `jobs` 0 one for them all.
pub(crate) fn program(
    units: Vec<ProgramUnit<'_>>,
    exports: &[String],
    target: hirsel_core_layout::Target,
    jobs: u32,
    reuse: &mut dyn FnMut(&str, &str, &[u8]) -> bool,
) -> Result<crate::Program, String> {
    emit(units, exports, true, target, jobs, reuse)
}

struct Receiver<'a> {
    reuse: &'a mut dyn FnMut(&str, &str, &[u8]) -> bool,
    program: crate::Program,
}
unsafe extern "C" fn reuse_part(
    receiver: *mut libc::c_void,
    name: *const u8,
    name_len: usize,
    machine: *const u8,
    machine_len: usize,
    bitcode: *const u8,
    len: usize,
) -> i32 {
    // SAFETY: `receiver` is the Receiver `emit` passes for this synchronous
    // call; the name, machine text and bitcode are borrowed for this callback
    // only.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        let text = |p, n| std::str::from_utf8(std::slice::from_raw_parts(p, n)).unwrap_or("");
        let name = text(name, name_len);
        let reused = (receiver.reuse)(
            name,
            text(machine, machine_len),
            std::slice::from_raw_parts(bitcode, len),
        );
        receiver.program.parts.push(crate::Part {
            name: name.to_owned(),
            reused,
        });
        i32::from(reused)
    }
}
unsafe extern "C" fn deliver_part(receiver: *mut libc::c_void, object: *const u8, len: usize) {
    // SAFETY: as in `reuse_part`; the object is copied before the callback returns.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        receiver
            .program
            .objects
            .push(std::slice::from_raw_parts(object, len).to_vec());
    }
}

fn emit(
    mut units: Vec<ProgramUnit<'_>>,
    exports: &[String],
    speed: bool,
    target: hirsel_core_layout::Target,
    jobs: u32,
    reuse: &mut dyn FnMut(&str, &str, &[u8]) -> bool,
) -> Result<crate::Program, String> {
    let triple = CString::new(target.triple()).unwrap();
    let cpu = CString::new(target.cpu()).unwrap();
    let backend = backend()?;
    let inputs: Vec<_> = units
        .iter_mut()
        .map(|u| match u {
            ProgramUnit::Native(module) => {
                let input = Input {
                    data: std::ptr::null(),
                    len: 0,
                    module: module.raw(),
                };
                module.consume();
                input
            }
            ProgramUnit::Text(ir) => Input {
                data: ir.as_ptr(),
                len: ir.len(),
                module: std::ptr::null_mut(),
            },
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
    let mut receiver = Receiver {
        reuse,
        program: crate::Program::default(),
    };
    // SAFETY: inputs, terminated export names and the receiver stay borrowed
    // through the synchronous C call, which calls back only during it. The
    // paired component allocator owns its output.
    unsafe {
        (backend.program)(
            inputs.as_ptr(),
            inputs.len(),
            pointers.as_ptr(),
            pointers.len(),
            u8::from(speed),
            triple.as_ptr(),
            cpu.as_ptr(),
            jobs,
            reuse_part,
            deliver_part,
            &mut receiver as *mut Receiver<'_> as *mut libc::c_void,
            &mut output,
        );
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(receiver.program)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}
