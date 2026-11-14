//! The same-build NoFolder construction table. The typed sink in
//! `core::sink::native` is its only caller; handles never outlive the
//! context that owns them.
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::rc::Rc;

pub(crate) type Raw = *mut c_void;
#[repr(C)]
pub(crate) struct Output {
    data: *mut u8,
    len: usize,
    failed: u8,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            data: std::ptr::null_mut(),
            len: 0,
            failed: 0,
        }
    }
}
/// Mirrors `HirselLlvmBuilderApi` in bridge/builder.h field for field.
#[repr(C)]
pub(crate) struct Api {
    pub context_new: unsafe extern "C" fn() -> Raw,
    pub context_drop: unsafe extern "C" fn(Raw),
    pub unit_new: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    pub unit_drop: unsafe extern "C" fn(Raw),
    pub unit_take: unsafe extern "C" fn(Raw, *mut Output) -> Raw,
    pub module_drop: unsafe extern "C" fn(Raw),
    pub print: unsafe extern "C" fn(Raw, *mut Output),
    pub print_text: unsafe extern "C" fn(*const c_char, usize, *mut Output),
    pub integer_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    pub float_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    pub pointer_type: unsafe extern "C" fn(Raw) -> Raw,
    pub void_type: unsafe extern "C" fn(Raw) -> Raw,
    pub metadata_type: unsafe extern "C" fn(Raw) -> Raw,
    pub array_type: unsafe extern "C" fn(Raw, u64) -> Raw,
    pub vector_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    pub struct_type: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    pub integer: unsafe extern "C" fn(Raw, u64, u64) -> Raw,
    pub poison: unsafe extern "C" fn(Raw) -> Raw,
    pub zero: unsafe extern "C" fn(Raw) -> Raw,
    pub constant_cast: unsafe extern "C" fn(u32, Raw, Raw) -> Raw,
    pub constant_add: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub constant_struct: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    pub constant_bytes: unsafe extern "C" fn(Raw, *const u8, usize) -> Raw,
    pub global_ref: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    pub global: unsafe extern "C" fn(Raw, *const c_char, usize, Raw, Raw, u32, u8, u8, u32, u8),
    pub function: unsafe extern "C" fn(Raw, *const c_char, usize, Raw, *const Raw, usize, u32) -> Raw,
    pub attribute: unsafe extern "C" fn(Raw, Raw, u32, u32, Raw, u64),
    pub string_attribute: unsafe extern "C" fn(Raw, Raw, *const c_char, usize, *const c_char, usize),
    pub finish_declaration: unsafe extern "C" fn(Raw),
    pub subprogram: unsafe extern "C" fn(Raw, Raw),
    pub begin_body: unsafe extern "C" fn(Raw, Raw),
    pub parameter: unsafe extern "C" fn(Raw, u32) -> Raw,
    pub set_name: unsafe extern "C" fn(Raw, *const c_char, usize),
    pub block: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    pub begin_block: unsafe extern "C" fn(Raw, Raw),
    pub block_placed: unsafe extern "C" fn(Raw) -> u8,
    pub placeholder: unsafe extern "C" fn(Raw) -> Raw,
    pub replace: unsafe extern "C" fn(Raw, Raw),
    pub alloca: unsafe extern "C" fn(Raw, Raw, u32) -> Raw,
    pub binary: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    pub cast: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    pub compare: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    pub select: unsafe extern "C" fn(Raw, Raw, Raw, Raw) -> Raw,
    pub fneg: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub load: unsafe extern "C" fn(Raw, Raw, Raw, u32, u32) -> Raw,
    pub store: unsafe extern "C" fn(Raw, Raw, Raw, u32, u32) -> Raw,
    pub gep: unsafe extern "C" fn(Raw, Raw, Raw, *const Raw, usize, u8) -> Raw,
    pub extract: unsafe extern "C" fn(Raw, Raw, u32) -> Raw,
    pub insert: unsafe extern "C" fn(Raw, Raw, Raw, u32) -> Raw,
    pub phi: unsafe extern "C" fn(Raw, Raw, *const Raw, *const Raw, usize) -> Raw,
    pub call: unsafe extern "C" fn(Raw, Raw, *const Raw, usize, Raw, *const Raw, usize, u8) -> Raw,
    pub compare_exchange: unsafe extern "C" fn(Raw, Raw, Raw, Raw) -> Raw,
    pub atomic_sub: unsafe extern "C" fn(Raw, Raw, Raw) -> Raw,
    pub fence: unsafe extern "C" fn(Raw, u32) -> Raw,
    pub branch: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub conditional: unsafe extern "C" fn(Raw, Raw, Raw, Raw) -> Raw,
    pub switch_value: unsafe extern "C" fn(Raw, Raw, Raw, *const Raw, *const Raw, usize) -> Raw,
    pub return_value: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub unreachable: unsafe extern "C" fn(Raw) -> Raw,
    pub module_asm: unsafe extern "C" fn(Raw, *const c_char, usize),
    pub metadata_ref: unsafe extern "C" fn(Raw, usize) -> Raw,
    pub metadata_string: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    pub metadata_integer: unsafe extern "C" fn(Raw, u32, u64) -> Raw,
    pub metadata_tuple: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    pub metadata_bind: unsafe extern "C" fn(Raw, usize, Raw),
    pub named_metadata: unsafe extern "C" fn(Raw, *const c_char, usize, *const Raw, usize),
    pub attach: unsafe extern "C" fn(Raw, Raw, u32, Raw),
    pub location: unsafe extern "C" fn(Raw, Raw, Raw),
    pub metadata_value: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub di_file: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    pub di_compile_unit: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub di_subroutine_type: unsafe extern "C" fn(Raw, Raw) -> Raw,
    pub di_subprogram:
        unsafe extern "C" fn(Raw, *const c_char, usize, *const c_char, usize, Raw, Raw, Raw) -> Raw,
    pub di_location: unsafe extern "C" fn(Raw, u32, u32, Raw) -> Raw,
    pub di_lexical_block_file: unsafe extern "C" fn(Raw, Raw, Raw) -> Raw,
}

pub(crate) struct Context {
    raw: Raw,
    pub(crate) api: &'static Api,
}
impl Context {
    pub fn new() -> Result<Self, String> {
        let api = crate::loader::construction_api()?;
        // SAFETY: this function table comes from the embedded same-build component.
        let raw = unsafe { (api.context_new)() };
        Ok(Self { raw, api })
    }
    /// A fresh unit whose module identifier is the parser's, "hirsel-core".
    pub fn unit(&self) -> Raw {
        // SAFETY: the context outlives the unit; callers own and free it.
        unsafe { (self.api.unit_new)(self.raw, c"hirsel-core".as_ptr(), 11) }
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: every unit and module borrowing this context is gone: units
        // are owned by sinks that borrow it, modules hold an Rc to it.
        unsafe { (self.api.context_drop)(self.raw) }
    }
}

/// A module built in a context retained until the destination loader consumes
/// the module handle.
pub(crate) struct NativeModule {
    context: Rc<Context>,
    raw: Raw,
    consumed: Cell<bool>,
}
impl NativeModule {
    pub(crate) fn new(context: Rc<Context>, raw: Raw) -> Self {
        Self {
            context,
            raw,
            consumed: Cell::new(false),
        }
    }
    pub(crate) fn raw(&self) -> Raw {
        self.raw
    }
    pub(crate) fn consume(&self) {
        self.consumed.set(true);
    }
}
impl Drop for NativeModule {
    fn drop(&mut self) {
        if !self.consumed.get() {
            // SAFETY: the handle was created by this same-build component and
            // remains exclusively owned by this wrapper.
            unsafe { (self.context.api.module_drop)(self.raw) }
        }
    }
}
/// The printed module of LLVM text parsed in its own fresh context.
pub(crate) fn print_text(text: &str) -> Result<String, String> {
    let api = crate::loader::construction_api()?;
    let mut out = Output::default();
    // SAFETY: text stays borrowed for the synchronous parser/printer call.
    unsafe { (api.print_text)(text.as_ptr().cast(), text.len(), &mut out) };
    output(out)
}
pub(crate) fn output_bytes(out: Output) -> (Vec<u8>, bool) {
    // SAFETY: the component returns an allocated byte buffer, including on failure.
    let bytes = unsafe { std::slice::from_raw_parts(out.data, out.len).to_vec() };
    // SAFETY: the paired allocator is the component's malloc/free ABI.
    unsafe { crate::loader::free_construction_output(out.data) };
    (bytes, out.failed != 0)
}
pub(crate) fn output(out: Output) -> Result<String, String> {
    let (bytes, failed) = output_bytes(out);
    let text = String::from_utf8(bytes).expect("LLVM prints UTF-8");
    if failed { Err(text) } else { Ok(text) }
}
