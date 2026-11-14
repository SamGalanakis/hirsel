//! Same-build NoFolder operations. Handles borrow the context that owns them;
//! CoreIR semantics and failure cleanup remain in the Rust lowering.
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::marker::PhantomData;
use std::rc::Rc;

type Raw = *mut c_void;
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct Local {
    pub kind: u8,
    pub index: usize,
}

#[cfg(test)]
pub(crate) fn printer_oracle_fixture() {
    let context = Context::new().unwrap();
    let mut unit = context.unit();
    let i64 = unit.integer_type(64);
    let i1 = unit.integer_type(1);
    let f = unit.function("fixture", i64, &[i64]);
    unit.define(f, false);
    let parameter = unit.parameter(f, 0, Local { kind: 4, index: 0 });
    let entry = unit.block("entry");
    let done = unit.block("done");
    let producer = unit.block("producer");
    unit.begin_block(entry);
    let one = unit.integer(i64, 1);
    let sum = unit.binary(0, parameter, one);
    unit.bind(Local { kind: 0, index: 0 }, sum);
    let yes = unit.integer(i1, 1);
    let poison = unit.poison(i64);
    let selected = unit.select(yes, sum, poison);
    unit.bind(Local { kind: 1, index: 0 }, selected);
    let forward = unit.local(i64, Local { kind: 0, index: 1 });
    unit.branch(producer);
    unit.begin_block(done);
    let slot = unit.alloca(i64, 8, Local { kind: 3, index: 0 });
    unit.store(forward, slot, 0);
    let loaded = unit.load(i64, slot, 0);
    unit.bind(Local { kind: 0, index: 2 }, loaded);
    unit.return_value(Some(loaded));
    unit.begin_block(producer);
    let value = unit.binary(0, selected, one);
    unit.bind(Local { kind: 0, index: 1 }, value);
    unit.branch(done);
    let text = "define i64 @fixture(i64 %p0) {\nentry:\n  %s0 = alloca i64, align 8\n  %t0 = add i64 %p0, 1\n  %v0 = select i1 true, i64 %t0, i64 poison\n  br label %producer\ndone:\n  store i64 %t1, ptr %s0\n  %t2 = load i64, ptr %s0\n  ret i64 %t2\nproducer:\n  %t1 = add i64 %v0, 1\n  br label %done\n}\n";
    assert_eq!(unit.print().unwrap(), context.print_text(text).unwrap());
}
#[repr(C)]
#[derive(Default)]
struct Output {
    data: *mut u8,
    len: usize,
    failed: u8,
}
#[repr(C)]
pub(crate) struct Api {
    context_new: unsafe extern "C" fn() -> Raw,
    context_drop: unsafe extern "C" fn(Raw),
    unit_new: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    unit_drop: unsafe extern "C" fn(Raw),
    unit_take: unsafe extern "C" fn(Raw) -> Raw,
    module_drop: unsafe extern "C" fn(Raw),
    module_parse: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    unit_bitcode: unsafe extern "C" fn(Raw, *mut Output),
    print: unsafe extern "C" fn(Raw, *mut Output),
    print_text: unsafe extern "C" fn(*const c_char, usize, *mut Output),
    integer_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    float_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    pointer_type: unsafe extern "C" fn(Raw) -> Raw,
    void_type: unsafe extern "C" fn(Raw) -> Raw,
    array_type: unsafe extern "C" fn(Raw, u64) -> Raw,
    struct_type: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    integer: unsafe extern "C" fn(Raw, u64, u64) -> Raw,
    poison: unsafe extern "C" fn(Raw) -> Raw,
    zero: unsafe extern "C" fn(Raw) -> Raw,
    function: unsafe extern "C" fn(Raw, *const c_char, usize, Raw, *const Raw, usize) -> Raw,
    define: unsafe extern "C" fn(Raw, Raw, u32),
    parameter: unsafe extern "C" fn(Raw, u32, Local) -> Raw,
    block: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    begin_block: unsafe extern "C" fn(Raw, Raw),
    name: unsafe extern "C" fn(Raw, Local),
    local: unsafe extern "C" fn(Raw, Raw, Local) -> Raw,
    bind: unsafe extern "C" fn(Raw, Local, Raw),
    alloca: unsafe extern "C" fn(Raw, Raw, u32, Local) -> Raw,
    binary: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    cast: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    compare: unsafe extern "C" fn(Raw, u32, Raw, Raw) -> Raw,
    select: unsafe extern "C" fn(Raw, Raw, Raw, Raw) -> Raw,
    load: unsafe extern "C" fn(Raw, Raw, Raw, u32) -> Raw,
    store: unsafe extern "C" fn(Raw, Raw, Raw, u32) -> Raw,
    gep: unsafe extern "C" fn(Raw, Raw, Raw, *const Raw, usize) -> Raw,
    extract: unsafe extern "C" fn(Raw, Raw, u32) -> Raw,
    insert: unsafe extern "C" fn(Raw, Raw, Raw, u32) -> Raw,
    phi: unsafe extern "C" fn(Raw, Raw, *const Raw, *const Raw, usize) -> Raw,
    call: unsafe extern "C" fn(Raw, Raw, *const Raw, usize, Raw, *const Raw, usize) -> Raw,
    branch: unsafe extern "C" fn(Raw, Raw),
    conditional: unsafe extern "C" fn(Raw, Raw, Raw, Raw) -> Raw,
    return_value: unsafe extern "C" fn(Raw, Raw),
    unreachable: unsafe extern "C" fn(Raw),
    vector_type: unsafe extern "C" fn(Raw, u32) -> Raw,
    constant_cast: unsafe extern "C" fn(u32, Raw, Raw) -> Raw,
    constant_add: unsafe extern "C" fn(Raw, Raw) -> Raw,
    global: unsafe extern "C" fn(Raw, *const c_char, usize, Raw) -> Raw,
    constant_struct: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    constant_array: unsafe extern "C" fn(Raw, *const Raw, usize) -> Raw,
    initialize_global: unsafe extern "C" fn(Raw, Raw, u32, u8, u8, u32),
    module_asm: unsafe extern "C" fn(Raw, *const c_char, usize),
    attribute: unsafe extern "C" fn(Raw, Raw, u32, u32, Raw),
    string_attribute: unsafe extern "C" fn(Raw, Raw, *const c_char, usize, *const c_char, usize),
    tail: unsafe extern "C" fn(Raw),
    fneg: unsafe extern "C" fn(Raw, Raw) -> Raw,
    atomic_load: unsafe extern "C" fn(Raw, Raw, Raw, u32, u32) -> Raw,
    atomic_store: unsafe extern "C" fn(Raw, Raw, Raw, u32, u32),
    atomic_rmw: unsafe extern "C" fn(Raw, u32, Raw, Raw, u32) -> Raw,
    compare_exchange: unsafe extern "C" fn(Raw, Raw, Raw, Raw, u32, u32) -> Raw,
    fence: unsafe extern "C" fn(Raw, u32),
    switch_value: unsafe extern "C" fn(Raw, Raw, Raw, *const Raw, *const Raw, usize),
    metadata_ref: unsafe extern "C" fn(Raw, usize) -> Raw,
    metadata_string: unsafe extern "C" fn(Raw, *const c_char, usize) -> Raw,
    metadata_integer: unsafe extern "C" fn(Raw, u32, u64) -> Raw,
    metadata_tuple: unsafe extern "C" fn(Raw, *const Raw, usize, u8) -> Raw,
    metadata_bind: unsafe extern "C" fn(Raw, usize, Raw),
    named_metadata: unsafe extern "C" fn(Raw, *const c_char, usize, *const Raw, usize),
    attach: unsafe extern "C" fn(Raw, Raw, *const c_char, usize, Raw),
    location: unsafe extern "C" fn(Raw, Raw),
    metadata_value: unsafe extern "C" fn(Raw, Raw) -> Raw,
}

pub(crate) struct Context {
    raw: Raw,
    api: &'static Api,
}
impl Context {
    pub fn new() -> Result<Self, String> {
        let api = crate::loader::construction_api()?;
        // SAFETY: this function table comes from the embedded same-build component.
        let raw = unsafe { (api.context_new)() };
        Ok(Self { raw, api })
    }
    pub fn unit(&self) -> Unit<'_> {
        // SAFETY: the borrowed context outlives this unit and all its handles.
        let raw = unsafe { (self.api.unit_new)(self.raw, c"hirsel-core".as_ptr(), 11) };
        Unit {
            raw,
            api: self.api,
            context: PhantomData,
        }
    }
    pub(crate) fn parse_module(self: &Rc<Self>, text: &str) -> Result<NativeModule, String> {
        // SAFETY: the source remains borrowed for the synchronous parse. The
        // returned module is owned by the same LLVM context retained in the
        // NativeModule wrapper.
        let raw = unsafe { (self.api.module_parse)(self.raw, text.as_ptr().cast(), text.len()) };
        if raw.is_null() {
            Err("LLVM text module parse failed".into())
        } else {
            Ok(NativeModule::new(self.clone(), raw))
        }
    }
    #[cfg(test)]
    pub fn print_text(&self, text: &str) -> Result<String, String> {
        let mut out = Output::default();
        // SAFETY: text stays borrowed for the synchronous parser/printer call.
        unsafe { (self.api.print_text)(text.as_ptr().cast(), text.len(), &mut out) };
        output(out)
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: units borrow Context, so no unit or value can survive this drop.
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
macro_rules! handle {
    ($name:ident) => {
        #[repr(transparent)]
        #[derive(Clone, Copy)]
        pub(crate) struct $name<'c>(Raw, PhantomData<&'c Context>);
    };
}
handle!(Type);
handle!(Value);
handle!(Block);
handle!(Metadata);
pub(crate) struct Unit<'c> {
    raw: Raw,
    api: &'static Api,
    context: PhantomData<&'c Context>,
}
impl Drop for Unit<'_> {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns the unit until take consumes it.
        unsafe { (self.api.unit_drop)(self.raw) }
    }
}
impl<'c> Unit<'c> {
    /// Consume the construction unit and transfer its LLVM module handle to
    /// the same-build linker. The context remains borrowed by the returned
    /// module until the linker consumes it.
    pub(crate) fn take(self) -> Raw {
        let unit = std::mem::ManuallyDrop::new(self);
        // SAFETY: ManuallyDrop prevents the unit destructor from running after
        // the component transfers ownership of the module.
        unsafe { (unit.api.unit_take)(unit.raw) }
    }
}
fn output(out: Output) -> Result<String, String> {
    // SAFETY: the component returns an allocated byte buffer, including on failure.
    let bytes = unsafe { std::slice::from_raw_parts(out.data, out.len).to_vec() };
    // SAFETY: the paired allocator is the component's malloc/free ABI.
    unsafe { crate::loader::free_construction_output(out.data) };
    let text = String::from_utf8(bytes).expect("LLVM prints UTF-8");
    if out.failed == 0 { Ok(text) } else { Err(text) }
}
impl<'c> Unit<'c> {
    pub fn bitcode(&self) -> Result<Vec<u8>, String> {
        let mut out = Output::default();
        // SAFETY: the component serializes the live module synchronously.
        unsafe { (self.api.unit_bitcode)(self.raw, &mut out) }
        let bytes = unsafe { std::slice::from_raw_parts(out.data, out.len).to_vec() };
        unsafe { crate::loader::free_construction_output(out.data) };
        if out.failed == 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
    pub fn print(&self) -> Result<String, String> {
        let mut out = Output::default();
        // SAFETY: this live unit owns the module throughout its synchronous print.
        unsafe { (self.api.print)(self.raw, &mut out) };
        output(out)
    }
    pub fn integer_type(&mut self, bits: u32) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(
            unsafe { (self.api.integer_type)(self.raw, bits) },
            PhantomData,
        )
    }
    pub fn float_type(&mut self, bits: u32) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(
            unsafe { (self.api.float_type)(self.raw, bits) },
            PhantomData,
        )
    }
    pub fn pointer_type(&mut self) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(unsafe { (self.api.pointer_type)(self.raw) }, PhantomData)
    }
    pub fn void_type(&mut self) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(unsafe { (self.api.void_type)(self.raw) }, PhantomData)
    }
    pub fn array_type(&mut self, element: Type<'c>, count: u64) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(
            unsafe { (self.api.array_type)(element.0, count) },
            PhantomData,
        )
    }
    pub fn integer(&mut self, ty: Type<'c>, bits: u128) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.integer)(ty.0, bits as u64, (bits >> 64) as u64) },
            PhantomData,
        )
    }
    pub fn poison(&mut self, ty: Type<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(unsafe { (self.api.poison)(ty.0) }, PhantomData)
    }
    pub fn zero(&mut self, ty: Type<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(unsafe { (self.api.zero)(ty.0) }, PhantomData)
    }
    pub fn parameter(&mut self, function: Value<'c>, index: u32, name: Local) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.parameter)(function.0, index, name) },
            PhantomData,
        )
    }
    pub fn local(&mut self, ty: Type<'c>, name: Local) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.local)(self.raw, ty.0, name) },
            PhantomData,
        )
    }
    pub fn alloca(&mut self, ty: Type<'c>, align: u32, name: Local) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.alloca)(self.raw, ty.0, align, name) },
            PhantomData,
        )
    }
    pub fn binary(&mut self, op: u32, left: Value<'c>, right: Value<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.binary)(self.raw, op, left.0, right.0) },
            PhantomData,
        )
    }
    pub fn cast(&mut self, op: u32, value: Value<'c>, ty: Type<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.cast)(self.raw, op, value.0, ty.0) },
            PhantomData,
        )
    }
    pub fn compare(&mut self, predicate: u32, left: Value<'c>, right: Value<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.compare)(self.raw, predicate, left.0, right.0) },
            PhantomData,
        )
    }
    pub fn select(&mut self, condition: Value<'c>, yes: Value<'c>, no: Value<'c>) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.select)(self.raw, condition.0, yes.0, no.0) },
            PhantomData,
        )
    }
    pub fn load(&mut self, ty: Type<'c>, pointer: Value<'c>, align: u32) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.load)(self.raw, ty.0, pointer.0, align) },
            PhantomData,
        )
    }
    pub fn gep(&mut self, ty: Type<'c>, pointer: Value<'c>, indices: &[Value<'c>]) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe {
                (self.api.gep)(
                    self.raw,
                    ty.0,
                    pointer.0,
                    indices.as_ptr().cast(),
                    indices.len(),
                )
            },
            PhantomData,
        )
    }
    pub fn extract(&mut self, value: Value<'c>, index: u32) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.extract)(self.raw, value.0, index) },
            PhantomData,
        )
    }
    pub fn insert(&mut self, aggregate: Value<'c>, value: Value<'c>, index: u32) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.insert)(self.raw, aggregate.0, value.0, index) },
            PhantomData,
        )
    }
    pub fn struct_type(&mut self, fields: &[Type<'c>]) -> Type<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Type(
            unsafe { (self.api.struct_type)(self.raw, fields.as_ptr().cast(), fields.len(), 0) },
            PhantomData,
        )
    }
    pub fn phi(&mut self, ty: Type<'c>, values: &[Value<'c>], blocks: &[Block<'c>]) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe {
                (self.api.phi)(
                    self.raw,
                    ty.0,
                    values.as_ptr().cast(),
                    blocks.as_ptr().cast(),
                    values.len(),
                )
            },
            PhantomData,
        )
    }
    pub fn function(&mut self, name: &str, result: Type<'c>, params: &[Type<'c>]) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe {
                (self.api.function)(
                    self.raw,
                    name.as_ptr().cast(),
                    name.len(),
                    result.0,
                    params.as_ptr().cast(),
                    params.len(),
                )
            },
            PhantomData,
        )
    }
    pub fn block(&mut self, name: &str) -> Block<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Block(
            unsafe { (self.api.block)(self.raw, name.as_ptr().cast(), name.len()) },
            PhantomData,
        )
    }
    pub fn call(
        &mut self,
        result: Type<'c>,
        params: &[Type<'c>],
        callee: Value<'c>,
        args: &[Value<'c>],
    ) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe {
                (self.api.call)(
                    self.raw,
                    result.0,
                    params.as_ptr().cast(),
                    params.len(),
                    callee.0,
                    args.as_ptr().cast(),
                    args.len(),
                )
            },
            PhantomData,
        )
    }
    pub fn define(&mut self, function: Value<'c>, internal: bool) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.define)(self.raw, function.0, u32::from(internal)) }
    }
    pub fn begin_block(&mut self, block: Block<'c>) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.begin_block)(self.raw, block.0) }
    }
    pub fn name(&mut self, value: Value<'c>, name: Local) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.name)(value.0, name) }
    }
    pub fn bind(&mut self, name: Local, value: Value<'c>) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.bind)(self.raw, name, value.0) }
    }
    pub fn store(&mut self, value: Value<'c>, pointer: Value<'c>, align: u32) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.store)(self.raw, value.0, pointer.0, align) },
            PhantomData,
        )
    }
    pub fn branch(&mut self, block: Block<'c>) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.branch)(self.raw, block.0) }
    }
    pub fn conditional(
        &mut self,
        condition: Value<'c>,
        yes: Block<'c>,
        no: Block<'c>,
    ) -> Value<'c> {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        Value(
            unsafe { (self.api.conditional)(self.raw, condition.0, yes.0, no.0) },
            PhantomData,
        )
    }
    pub fn return_value(&mut self, value: Option<Value<'c>>) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.return_value)(self.raw, value.map_or(std::ptr::null_mut(), |v| v.0)) }
    }
    pub fn unreachable(&mut self) {
        // SAFETY: handles belong to this borrowed context; slices live through the call.
        unsafe { (self.api.unreachable)(self.raw) }
    }
}

impl<'c> Unit<'c> {
    pub fn vector_type(&mut self, element: Type<'c>, count: u32) -> Type<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Type(
            unsafe { (self.api.vector_type)(element.0, count) },
            PhantomData,
        )
    }
    pub fn packed_struct_type(&mut self, fields: &[Type<'c>]) -> Type<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Type(
            unsafe { (self.api.struct_type)(self.raw, fields.as_ptr().cast(), fields.len(), 1) },
            PhantomData,
        )
    }
    pub fn constant_cast(&mut self, op: u32, value: Value<'c>, ty: Type<'c>) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.constant_cast)(op, value.0, ty.0) },
            PhantomData,
        )
    }
    pub fn constant_add(&mut self, left: Value<'c>, right: Value<'c>) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.constant_add)(left.0, right.0) },
            PhantomData,
        )
    }
    pub fn global(&mut self, name: &str, ty: Type<'c>) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.global)(self.raw, name.as_ptr().cast(), name.len(), ty.0) },
            PhantomData,
        )
    }
    pub fn constant_struct(&mut self, values: &[Value<'c>], packed: bool) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe {
                (self.api.constant_struct)(
                    self.raw,
                    values.as_ptr().cast(),
                    values.len(),
                    u8::from(packed),
                )
            },
            PhantomData,
        )
    }
    pub fn constant_array(&mut self, element: Type<'c>, values: &[Value<'c>]) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.constant_array)(element.0, values.as_ptr().cast(), values.len()) },
            PhantomData,
        )
    }
    pub fn initialize_global(
        &mut self,
        global: Value<'c>,
        init: Option<Value<'c>>,
        linkage: u32,
        constant: bool,
        externally_initialized: bool,
        align: u32,
    ) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.initialize_global)(
                global.0,
                init.map_or(std::ptr::null_mut(), |v| v.0),
                linkage,
                u8::from(constant),
                u8::from(externally_initialized),
                align,
            )
        }
    }
    pub fn module_asm(&mut self, text: &str) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.module_asm)(self.raw, text.as_ptr().cast(), text.len()) }
    }
    pub fn attribute(&mut self, value: Value<'c>, index: u32, kind: u32, ty: Option<Type<'c>>) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.attribute)(
                self.raw,
                value.0,
                index,
                kind,
                ty.map_or(std::ptr::null_mut(), |t| t.0),
            )
        }
    }
    pub fn string_attribute(&mut self, value: Value<'c>, name: &str, text: &str) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.string_attribute)(
                self.raw,
                value.0,
                name.as_ptr().cast(),
                name.len(),
                text.as_ptr().cast(),
                text.len(),
            )
        }
    }
    pub fn tail(&mut self, value: Value<'c>) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.tail)(value.0) }
    }
    pub fn fneg(&mut self, value: Value<'c>) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(unsafe { (self.api.fneg)(self.raw, value.0) }, PhantomData)
    }
    pub fn atomic_load(
        &mut self,
        ty: Type<'c>,
        pointer: Value<'c>,
        align: u32,
        ordering: u32,
    ) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.atomic_load)(self.raw, ty.0, pointer.0, align, ordering) },
            PhantomData,
        )
    }
    pub fn atomic_store(
        &mut self,
        value: Value<'c>,
        pointer: Value<'c>,
        align: u32,
        ordering: u32,
    ) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.atomic_store)(self.raw, value.0, pointer.0, align, ordering) }
    }
    pub fn atomic_rmw(
        &mut self,
        op: u32,
        pointer: Value<'c>,
        value: Value<'c>,
        ordering: u32,
    ) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.atomic_rmw)(self.raw, op, pointer.0, value.0, ordering) },
            PhantomData,
        )
    }
    pub fn compare_exchange(
        &mut self,
        pointer: Value<'c>,
        expected: Value<'c>,
        desired: Value<'c>,
        success: u32,
        failure: u32,
    ) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe {
                (self.api.compare_exchange)(
                    self.raw, pointer.0, expected.0, desired.0, success, failure,
                )
            },
            PhantomData,
        )
    }
    pub fn fence(&mut self, ordering: u32) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.fence)(self.raw, ordering) }
    }
    pub fn switch_value(
        &mut self,
        value: Value<'c>,
        default: Block<'c>,
        values: &[Value<'c>],
        blocks: &[Block<'c>],
    ) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.switch_value)(
                self.raw,
                value.0,
                default.0,
                values.as_ptr().cast(),
                blocks.as_ptr().cast(),
                values.len(),
            )
        }
    }
    pub fn metadata_ref(&mut self, id: usize) -> Metadata<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Metadata(
            unsafe { (self.api.metadata_ref)(self.raw, id) },
            PhantomData,
        )
    }
    pub fn metadata_string(&mut self, text: &str) -> Metadata<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Metadata(
            unsafe { (self.api.metadata_string)(self.raw, text.as_ptr().cast(), text.len()) },
            PhantomData,
        )
    }
    pub fn metadata_integer(&mut self, bits: u32, value: u64) -> Metadata<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Metadata(
            unsafe { (self.api.metadata_integer)(self.raw, bits, value) },
            PhantomData,
        )
    }
    pub fn metadata_tuple(&mut self, values: &[Metadata<'c>], distinct: bool) -> Metadata<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Metadata(
            unsafe {
                (self.api.metadata_tuple)(
                    self.raw,
                    values.as_ptr().cast(),
                    values.len(),
                    u8::from(distinct),
                )
            },
            PhantomData,
        )
    }
    pub fn metadata_bind(&mut self, id: usize, value: Metadata<'c>) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.metadata_bind)(self.raw, id, value.0) }
    }
    pub fn named_metadata(&mut self, name: &str, values: &[Metadata<'c>]) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.named_metadata)(
                self.raw,
                name.as_ptr().cast(),
                name.len(),
                values.as_ptr().cast(),
                values.len(),
            )
        }
    }
    pub fn attach(&mut self, value: Value<'c>, name: &str, metadata: Metadata<'c>) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe {
            (self.api.attach)(
                self.raw,
                value.0,
                name.as_ptr().cast(),
                name.len(),
                metadata.0,
            )
        }
    }
    pub fn location(&mut self, location: Option<Metadata<'c>>) {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        unsafe { (self.api.location)(self.raw, location.map_or(std::ptr::null_mut(), |m| m.0)) }
    }
    pub fn metadata_value(&mut self, metadata: Metadata<'c>) -> Value<'c> {
        // SAFETY: context-bound handles and borrowed slices live through this call.
        Value(
            unsafe { (self.api.metadata_value)(self.raw, metadata.0) },
            PhantomData,
        )
    }
}
