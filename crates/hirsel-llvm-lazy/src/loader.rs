//! The component is part of the executable, rather than an independently
//! replaceable plugin. Its bytes are mapped only when LLVM emits an object.
//! libLLVM is not part of the executable: [`llvm_library`] names the one file this
//! process loads, so an LLVM product's key can name that file's content.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/component.rs"));
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
type Code = unsafe extern "C" fn(*mut libc::c_void, *const u8, usize, i32, *const u8, usize);
type CounterRangeReport = unsafe extern "C" fn(*mut libc::c_void, *const u8, usize, libc::c_uint);
type SourceRemark = unsafe extern "C" fn(
    *mut libc::c_void,
    libc::c_uint,
    *const u8,
    usize,
    *const u8,
    usize,
    *const u8,
    usize,
    libc::c_uint,
);
type FindTail = unsafe extern "C" fn(
    *mut libc::c_void,
    *const u8,
    usize,
    *const u8,
    usize,
    *mut *const u8,
    *mut usize,
) -> i32;
type KeepTail = unsafe extern "C" fn(
    *mut libc::c_void,
    *const u8,
    usize,
    *const u8,
    usize,
    *const u8,
    usize,
    *const u8,
    usize,
);
type Program = unsafe extern "C" fn(
    *const Input,
    usize,
    *const *const libc::c_char,
    usize,
    u8,
    *const libc::c_char,
    *const libc::c_char,
    *const libc::c_char,
    u32,
    FindTail,
    KeepTail,
    Reuse,
    Deliver,
    Option<CounterRangeReport>,
    Option<SourceRemark>,
    *mut libc::c_void,
    *const u8,
    usize,
    usize,
    *const libc::c_char,
    Option<Deliver>,
    Option<Code>,
    *mut Output,
);
type Generate = unsafe extern "C" fn(
    *const Input,
    usize,
    *const libc::c_char,
    *const libc::c_char,
    *const libc::c_char,
    u32,
    Deliver,
    *mut libc::c_void,
    *mut Output,
);
type StdObjectCodec = unsafe extern "C" fn(*const u8, usize, usize, *mut Output);
type JoinLent = unsafe extern "C" fn(*const Input, usize, *mut Output);
pub(crate) enum ProgramUnit {
    Native(crate::construction::NativeModule),
    Text(String),
}
struct Backend {
    compile: Compile,
    program: Program,
    generate: Generate,
    join_lent: JoinLent,
    std_object_codec: StdObjectCodec,
    free: Free,
    construction: &'static crate::construction::Api,
    _image: crate::host::Component,
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
    open_with(name, libc::RTLD_LOCAL)
}

fn open_with(name: &CStr, scope: libc::c_int) -> Result<*mut libc::c_void, String> {
    // SAFETY: the name is terminated. Handles remain loaded for process life,
    // like rustc's codegen backend, so no callback can outlive its code.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | scope) };
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
    let executable = crate::compiler_executable().map_err(|error| error.to_string())?;
    let origin = executable
        .parent()
        .ok_or("compiler has no installation directory")?;
    let bundled = origin.join("lib").join(LLVM_LIBRARY);
    let prefix = origin.join("../lib").join(LLVM_LIBRARY);
    let buck = BUCK_ENGINE.map(|path| origin.join(path));
    let (path, expected) = if bundled.exists() {
        (bundled, LLVM_BUILD_ID.into())
    } else if origin.file_name().is_some_and(|name| name == "bin") && prefix.exists() {
        (prefix, LLVM_BUILD_ID.into())
    } else if let Some(buck) = buck.filter(|path| path.is_file()) {
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
    let image = object::File::parse(&cache).map_err(|cause| error(&cause))?;
    // The link-time identity the build script read: an ELF build ID or a Mach-O LC_UUID.
    let uuid = image.mach_uuid().map_err(|cause| error(&cause))?;
    let id = match image.build_id().map_err(|cause| error(&cause))? {
        Some(id) => id,
        None => uuid
            .as_ref()
            .map(<[u8; 16]>::as_slice)
            .ok_or_else(|| error(&"no build ID"))?,
    };
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
/// An allocation LLVM could not make ends the process as the memory guard
/// ends it for a refused Rust allocation.
unsafe extern "C" fn llvm_out_of_memory(reason: *const libc::c_char) -> ! {
    // SAFETY: LLVM passes a NUL-terminated reason that outlives the call.
    let reason = unsafe { std::ffi::CStr::from_ptr(reason) };
    hirsel_guard::foreign_exhausted(b"LLVM", reason.to_bytes())
}
fn load() -> Result<Backend, String> {
    let llvm = CString::new(llvm_library()?.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    open_with(&llvm, crate::host::LLVM_SCOPE)?;
    let (handle, image) = crate::host::open_component(COMPONENT, open)?;
    // SAFETY: these two exports and C layouts are defined by the embedded
    // engine.c built in this same artifact. No foreign component is selected.
    let compile = unsafe { libc::dlsym(handle, c"hirsel_llvm_compile_v2".as_ptr()) };
    let free = unsafe { libc::dlsym(handle, c"hirsel_llvm_output_free_v1".as_ptr()) };
    let program = unsafe { libc::dlsym(handle, c"hirsel_llvm_program_v11".as_ptr()) };
    let generate = unsafe { libc::dlsym(handle, c"hirsel_llvm_generate_v1".as_ptr()) };
    let join_lent = unsafe { libc::dlsym(handle, c"hirsel_llvm_join_lent_v1".as_ptr()) };
    let std_object_codec =
        unsafe { libc::dlsym(handle, c"hirsel_llvm_std_object_codec_v1".as_ptr()) };
    // SAFETY: the table getter is part of the same embedded component ABI.
    let construction = unsafe { libc::dlsym(handle, c"hirsel_llvm_builder_v1".as_ptr()) };
    let out_of_memory = unsafe { libc::dlsym(handle, c"hirsel_llvm_out_of_memory_v1".as_ptr()) };
    if compile.is_null()
        || free.is_null()
        || program.is_null()
        || generate.is_null()
        || join_lent.is_null()
        || std_object_codec.is_null()
        || construction.is_null()
        || out_of_memory.is_null()
    {
        return Err(dl_error());
    }
    // SAFETY: the same-build installer takes a handler that never returns,
    // before any LLVM work runs.
    unsafe {
        let install = std::mem::transmute::<
            *mut libc::c_void,
            unsafe extern "C" fn(unsafe extern "C" fn(*const libc::c_char) -> !),
        >(out_of_memory);
        install(llvm_out_of_memory);
    }
    Ok(Backend {
        // SAFETY: same-build functions have the signatures above. They own no
        // Rust values across the C boundary; output is copied then freed there.
        compile: unsafe { std::mem::transmute::<*mut libc::c_void, Compile>(compile) },
        // SAFETY: this export is built with the Input/Output C layouts above.
        program: unsafe { std::mem::transmute::<*mut libc::c_void, Program>(program) },
        // SAFETY: as `program`'s.
        generate: unsafe { std::mem::transmute::<*mut libc::c_void, Generate>(generate) },
        join_lent: unsafe { std::mem::transmute::<*mut libc::c_void, JoinLent>(join_lent) },
        std_object_codec: unsafe {
            std::mem::transmute::<*mut libc::c_void, StdObjectCodec>(std_object_codec)
        },
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
    let triple = CString::new(target.llvm_triple()).unwrap();
    let cpu = CString::new(target.cpu()).unwrap();
    let features = CString::new(target.llvm_features()).unwrap();
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
            features.as_ptr(),
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
        &mut crate::Unheld,
        None,
        crate::Lend::None,
    )?;
    Ok(program
        .objects
        .pop()
        .expect("a standalone module is one object"))
}

/// A program's parts, in order, and the objects generated for the parts
/// `held` did not hold: one per part on `jobs` threads, or with
/// `jobs` 0 one for them all.
pub(crate) fn program(
    units: Vec<ProgramUnit>,
    exports: &[String],
    target: hirsel_core_layout::Target,
    jobs: u32,
    held: &mut dyn crate::Held,
    folds: Option<&mut crate::CounterRangeFolds>,
    lend: crate::Lend<'_>,
) -> Result<crate::Program, String> {
    emit(units, exports, true, target, jobs, held, folds, lend)
}

struct Receiver<'a> {
    held: &'a mut dyn crate::Held,
    program: crate::Program,
    folds: Option<&'a mut crate::CounterRangeFolds>,
    /// The last tail `find_tail` answered with, borrowed by the engine until
    /// its next call.
    tail: Vec<u8>,
}
unsafe extern "C" fn reuse_part(
    receiver: *mut libc::c_void,
    name: *const u8,
    name_len: usize,
    machine: *const u8,
    machine_len: usize,
    ir: *const u8,
    len: usize,
) -> i32 {
    // SAFETY: `receiver` is the Receiver `emit` passes for this synchronous
    // call; the name, machine text and key are borrowed for this callback
    // only.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        let text = |p, n| std::str::from_utf8(std::slice::from_raw_parts(p, n)).unwrap_or("");
        let name = text(name, name_len);
        let ir = ir_key(ir, len);
        let reused = receiver.held.part(name, text(machine, machine_len), &ir);
        receiver.program.parts.push(crate::Part {
            name: name.to_owned(),
            reused,
            ir,
        });
        i32::from(reused)
    }
}
unsafe extern "C" fn find_tail(
    receiver: *mut libc::c_void,
    key: *const u8,
    key_len: usize,
    ir: *const u8,
    len: usize,
    tail: *mut *const u8,
    tail_len: *mut usize,
) -> i32 {
    // SAFETY: as in `reuse_part`; the answer stays in the receiver until the
    // engine's next call, which reads it before then.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        let key = std::slice::from_raw_parts(key, key_len);
        let Some(held) = receiver.held.tail(key, &ir_key(ir, len)) else {
            // 2: not held, and `keep_tail` is to get the part's bitcode.
            return if receiver.held.witness() { 2 } else { 0 };
        };
        receiver.tail = held;
        *tail = receiver.tail.as_ptr();
        *tail_len = receiver.tail.len();
        1
    }
}
unsafe extern "C" fn keep_tail(
    receiver: *mut libc::c_void,
    key: *const u8,
    key_len: usize,
    ir: *const u8,
    len: usize,
    input: *const u8,
    input_len: usize,
    tail: *const u8,
    tail_len: usize,
) {
    // SAFETY: as in `reuse_part`; `input` is empty (and may be null) unless
    // `find_tail` asked for it.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        let input = if input_len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(input, input_len)
        };
        receiver.held.keep_tail(
            std::slice::from_raw_parts(key, key_len),
            &ir_key(ir, len),
            input,
            std::slice::from_raw_parts(tail, tail_len),
        );
    }
}
/// A part's key as the engine passes it.
///
/// # Safety
/// `ir` points at `len` readable bytes.
unsafe fn ir_key(ir: *const u8, len: usize) -> [u8; 32] {
    // SAFETY: the caller's.
    let bytes = unsafe { std::slice::from_raw_parts(ir, len) };
    bytes.try_into().expect("a part key is 32 bytes")
}
unsafe extern "C" fn lend_bodies(receiver: *mut libc::c_void, lent: *const u8, len: usize) {
    // SAFETY: as in `deliver_part`.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        receiver.program.lent = std::slice::from_raw_parts(lent, len).to_vec();
    }
}
unsafe extern "C" fn defer_part(
    receiver: *mut libc::c_void,
    name: *const u8,
    name_len: usize,
    strong: i32,
    code: *const u8,
    len: usize,
) {
    // SAFETY: as in `deliver_part`; the name and bitcode are copied before
    // the callback returns.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        let name = std::slice::from_raw_parts(name, name_len);
        receiver.program.deferred.push(crate::Deferred {
            name: String::from_utf8_lossy(name).into_owned(),
            strong: strong != 0,
            code: std::slice::from_raw_parts(code, len).to_vec(),
        });
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

unsafe extern "C" fn report_counter_range(
    receiver: *mut libc::c_void,
    name: *const u8,
    len: usize,
    removed: libc::c_uint,
) {
    // SAFETY: the optimizer calls synchronously with this receiver and the
    // function symbol's bytes borrowed until the callback returns.
    unsafe {
        let receiver = &mut *(receiver as *mut Receiver<'_>);
        if let Some(folds) = &mut receiver.folds {
            let name = std::slice::from_raw_parts(name, len);
            *folds.entry(name.to_vec()).or_default() += removed as usize;
        }
    }
}

unsafe extern "C" fn report_source_remark(
    receiver: *mut libc::c_void,
    kind: libc::c_uint,
    definition: *const u8,
    definition_len: usize,
    linkage: *const u8,
    linkage_len: usize,
    owner: *const u8,
    owner_len: usize,
    start: libc::c_uint,
) {
    use hirsel_core_ids::DefPath;
    use hirsel_core_ir::source::{CodegenRemark, RemarkKind};
    // SAFETY: the engine lends these spans and the receiver in this callback.
    unsafe {
        let receiver = &mut *receiver.cast::<Receiver<'_>>();
        let text = |bytes, len| std::str::from_utf8(std::slice::from_raw_parts(bytes, len)).ok();
        let Some(definition) = text(definition, definition_len).and_then(DefPath::from_full) else {
            return;
        };
        let Some(owner) = text(owner, owner_len).and_then(DefPath::from_full) else {
            return;
        };
        let Some(linkage) = text(linkage, linkage_len) else {
            return;
        };
        let kind = match kind {
            0 => RemarkKind::BoundsCheck,
            1 => RemarkKind::OverflowCheck,
            2 => RemarkKind::StepSync,
            _ => unreachable!("LLVM source remark kind"),
        };
        receiver.program.remarks.push(CodegenRemark {
            definition,
            linkage: linkage.to_owned(),
            owner,
            start,
            kind,
        });
    }
}

fn emit(
    mut units: Vec<ProgramUnit>,
    exports: &[String],
    speed: bool,
    target: hirsel_core_layout::Target,
    jobs: u32,
    held: &mut dyn crate::Held,
    folds: Option<&mut crate::CounterRangeFolds>,
    lend: crate::Lend<'_>,
) -> Result<crate::Program, String> {
    // A library defers its parts: a program over it generates the parts it
    // links ([`generate`]).
    let (lent, limit, namespace, lend_to, defer): (_, _, _, Option<Deliver>, Option<Code>) =
        match lend {
            crate::Lend::None => (&[][..], 0, "", None, None),
            crate::Lend::Borrow(lent) => (lent, 0, "", None, None),
            crate::Lend::Lend { limit, namespace } => {
                (&[], limit, namespace, Some(lend_bodies), Some(defer_part))
            }
        };
    let own = CString::new(namespace).unwrap();
    let triple = CString::new(target.llvm_triple()).unwrap();
    let cpu = CString::new(target.cpu()).unwrap();
    let features = CString::new(target.llvm_features()).unwrap();
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
    let report = folds
        .as_ref()
        .map(|_| report_counter_range as CounterRangeReport);
    let mut receiver = Receiver {
        held,
        program: crate::Program::default(),
        folds,
        tail: Vec::new(),
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
            features.as_ptr(),
            jobs,
            find_tail,
            keep_tail,
            reuse_part,
            deliver_part,
            report,
            Some(report_source_remark),
            &mut receiver as *mut Receiver<'_> as *mut libc::c_void,
            lent.as_ptr(),
            lent.len(),
            limit,
            own.as_ptr(),
            lend_to,
            defer,
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

pub(crate) fn join_lent(modules: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let backend = backend()?;
    let inputs: Vec<_> = modules
        .iter()
        .map(|module| Input {
            data: module.as_ptr(),
            len: module.len(),
            module: std::ptr::null_mut(),
        })
        .collect();
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: the slices remain live during the synchronous call, and the
    // same engine allocates and frees the returned buffer.
    unsafe {
        (backend.join_lent)(inputs.as_ptr(), inputs.len(), &mut output);
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

pub(crate) fn std_object_codec(bytes: &[u8], decoded_size: usize) -> Result<Vec<u8>, String> {
    let backend = backend()?;
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: bytes and output are borrowed for the synchronous call. The
    // allocating component also frees the output.
    unsafe {
        (backend.std_object_codec)(bytes.as_ptr(), bytes.len(), decoded_size, &mut output);
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

/// The objects of the parts whose bitcode `codes` holds ([`crate::Deferred`]),
/// in order, generated for `target` on up to `jobs` threads.
pub(crate) fn generate(
    codes: &[&[u8]],
    target: hirsel_core_layout::Target,
    jobs: u32,
) -> Result<Vec<Vec<u8>>, String> {
    let triple = CString::new(target.llvm_triple()).unwrap();
    let cpu = CString::new(target.cpu()).unwrap();
    let features = CString::new(target.llvm_features()).unwrap();
    let backend = backend()?;
    let inputs: Vec<_> = codes
        .iter()
        .map(|code| Input {
            data: code.as_ptr(),
            len: code.len(),
            module: std::ptr::null_mut(),
        })
        .collect();
    let mut objects: Vec<Vec<u8>> = Vec::with_capacity(codes.len());
    unsafe extern "C" fn deliver(receiver: *mut libc::c_void, object: *const u8, len: usize) {
        // SAFETY: `receiver` is the vector `generate` passes for this
        // synchronous call; the object is copied before the callback returns.
        unsafe {
            (*(receiver as *mut Vec<Vec<u8>>))
                .push(std::slice::from_raw_parts(object, len).to_vec());
        }
    }
    let mut output = Output {
        data: std::ptr::null_mut(),
        len: 0,
        failed: 0,
    };
    // SAFETY: the bitcode, the terminated target strings and the receiver
    // stay borrowed through the synchronous C call, which delivers only
    // during it. The paired component allocator owns its output.
    unsafe {
        (backend.generate)(
            inputs.as_ptr(),
            inputs.len(),
            triple.as_ptr(),
            cpu.as_ptr(),
            features.as_ptr(),
            jobs,
            deliver,
            &mut objects as *mut Vec<Vec<u8>> as *mut libc::c_void,
            &mut output,
        );
        let bytes = std::slice::from_raw_parts(output.data, output.len).to_vec();
        (backend.free)(output.data);
        if output.failed == 0 {
            Ok(objects)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}
