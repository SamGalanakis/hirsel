//! The product sink: each item and instruction becomes the LLVM object the
//! assembly parser would build for the text producer's line, in the same
//! order. Forward locals are typed placeholders replaced at their definition.
use super::*;
use crate::construction::{self as llvm, Api, Output, Raw};
use std::collections::HashMap;
use std::rc::Rc;

const RETURN: u32 = 0;
const FUNCTION: u32 = u32::MAX;
const ZEXT: u32 = 0;
const SEXT: u32 = 1;
const NOALIAS: u32 = 2;
const READONLY: u32 = 3;
const SRET: u32 = 4;
const BYVAL: u32 = 5;
const ALWAYS_INLINE: u32 = 6;
const NO_INLINE: u32 = 7;
const COLD: u32 = 8;
const NONNULL: u32 = 9;
const NOUNWIND: u32 = 10;
const ALIGN: u32 = 11;
const ARGMEM_READ: u32 = 12;
const ALIAS_SCOPE: u32 = 0;
const NOALIAS_SCOPE: u32 = 1;
const INVARIANT: u32 = 2;
const PROF: u32 = 3;

pub(crate) struct NativeSink {
    context: Rc<llvm::Context>,
    api: &'static Api,
    unit: Raw,
    types: HashMap<Type, Raw>,
    locals: HashMap<Local, Raw>,
    pending: HashMap<Local, Raw>,
    blocks: HashMap<Label, Raw>,
    name: String,
}
impl Drop for NativeSink {
    fn drop(&mut self) {
        if !self.unit.is_null() {
            // SAFETY: the sink exclusively owns its unit until `finish`.
            unsafe { (self.api.unit_drop)(self.unit) }
        }
    }
}
impl NativeSink {
    pub fn new(context: Rc<llvm::Context>) -> Self {
        Self {
            api: context.api,
            unit: context.unit(),
            context,
            types: HashMap::new(),
            locals: HashMap::new(),
            pending: HashMap::new(),
            blocks: HashMap::new(),
            name: String::new(),
        }
    }
    /// The finished module, or the first global referenced but never defined.
    pub fn finish(mut self) -> std::result::Result<llvm::NativeModule, String> {
        let unit = std::mem::replace(&mut self.unit, std::ptr::null_mut());
        let mut out = Output::default();
        // SAFETY: `take` consumes the unit whether or not it succeeds.
        let module = unsafe { (self.api.unit_take)(unit, &mut out) };
        llvm::output(out)?;
        Ok(llvm::NativeModule::new(self.context.clone(), module))
    }
    pub fn print(&self) -> std::result::Result<String, String> {
        let mut out = Output::default();
        // SAFETY: the live unit owns its module through the synchronous print.
        unsafe { (self.api.print)(self.unit, &mut out) };
        llvm::output(out)
    }
    fn ty(&mut self, ty: &Type) -> Raw {
        if let Some(&t) = self.types.get(ty) {
            return t;
        }
        let (api, u) = (self.api, self.unit);
        // SAFETY (this block): type handles belong to the unit's live context;
        // field arrays outlive each synchronous call.
        let t = unsafe {
            match ty {
                Type::Void => (api.void_type)(u),
                Type::Int(bits) => (api.integer_type)(u, *bits),
                Type::Float => (api.float_type)(u, 32),
                Type::Double => (api.float_type)(u, 64),
                Type::Ptr => (api.pointer_type)(u),
                Type::Metadata => (api.metadata_type)(u),
                Type::Array(count, element) => {
                    let element = self.ty(element);
                    (api.array_type)(element, *count)
                }
                Type::Vector(count, element) => {
                    let element = self.ty(element);
                    (api.vector_type)(element, *count)
                }
                Type::Struct(fields, packed) => {
                    let fields: Vec<_> = fields.iter().map(|f| self.ty(f)).collect();
                    (api.struct_type)(u, fields.as_ptr(), fields.len(), u8::from(*packed))
                }
            }
        };
        self.types.insert(ty.clone(), t);
        t
    }
    fn integer(&mut self, ty: &Type, n: i128) -> Raw {
        let t = self.ty(ty);
        let bits = n as u128;
        // SAFETY: an integer constant of a live integer type.
        unsafe { (self.api.integer)(t, bits as u64, (bits >> 64) as u64) }
    }
    fn global_ref(&mut self, name: &str) -> Raw {
        // SAFETY: the name is borrowed through the synchronous lookup.
        unsafe { (self.api.global_ref)(self.unit, name.as_ptr().cast(), name.len()) }
    }
    fn local(&mut self, local: Local, ty: &Type) -> Raw {
        if let Some(&v) = self.locals.get(&local) {
            return v;
        }
        if let Some(&v) = self.pending.get(&local) {
            return v;
        }
        let t = self.ty(ty);
        // SAFETY: a detached typed placeholder, replaced at the definition.
        let v = unsafe { (self.api.placeholder)(t) };
        self.pending.insert(local, v);
        v
    }
    fn define(&mut self, local: Local, value: Raw) {
        self.name.clear();
        local.write_name(&mut self.name).unwrap();
        // SAFETY: naming and replacing values of this unit's current function.
        unsafe {
            (self.api.set_name)(value, self.name.as_ptr().cast(), self.name.len());
            if let Some(pending) = self.pending.remove(&local) {
                (self.api.replace)(pending, value);
            }
        }
        let previous = self.locals.insert(local, value);
        debug_assert!(previous.is_none(), "{local} is defined twice");
    }
    fn value(&mut self, value: &Value, ty: &Type) -> Raw {
        let api = self.api;
        // SAFETY (this block): constants of live types and globals of this unit.
        unsafe {
            match value {
                Value::Local(local) => self.local(*local, ty),
                Value::Int(n) => self.integer(ty, *n),
                Value::Bool(b) => self.integer(&Type::I1, i128::from(*b)),
                Value::Null | Value::Zero => (api.zero)(self.ty(ty)),
                Value::Poison => (api.poison)(self.ty(ty)),
                Value::Global(name) => self.global_ref(name),
                Value::Bitcast { from, bits, to } => {
                    let bits = self.integer(from, i128::from(*bits));
                    (api.constant_cast)(Cast::Bitcast as u32, bits, self.ty(to))
                }
                Value::IntToPtr(bits) => {
                    let bits = self.integer(&Type::I64, i128::from(*bits));
                    (api.constant_cast)(Cast::IntToPtr as u32, bits, self.ty(&Type::Ptr))
                }
                Value::PtrToInt(name) => {
                    let global = self.global_ref(name);
                    (api.constant_cast)(Cast::PtrToInt as u32, global, self.ty(&Type::I64))
                }
                Value::Add(left, right) => {
                    let left = self.value(left, &Type::I64);
                    let right = self.integer(&Type::I64, i128::from(*right));
                    (api.constant_add)(left, right)
                }
                Value::Struct { packed, fields } => {
                    let fields: Vec<_> = fields.iter().map(|(t, v)| self.value(v, t)).collect();
                    (api.constant_struct)(self.unit, fields.as_ptr(), fields.len(), u8::from(*packed))
                }
                Value::Bytes(bytes) => (api.constant_bytes)(self.unit, bytes.as_ptr(), bytes.len()),
                Value::Metadata(ids) => {
                    let tuple = self.scope_list(ids);
                    (api.metadata_value)(self.unit, tuple)
                }
            }
        }
    }
    fn block_ref(&mut self, label: Label) -> Raw {
        if let Some(&b) = self.blocks.get(&label) {
            return b;
        }
        self.name.clear();
        label.write_name(&mut self.name).unwrap();
        // SAFETY: a detached block, placed when the producer begins it.
        let b = unsafe { (self.api.block)(self.unit, self.name.as_ptr().cast(), self.name.len()) };
        self.blocks.insert(label, b);
        b
    }
    fn md(&mut self, id: usize) -> Raw {
        // SAFETY: the unit's node for this producer identifier.
        unsafe { (self.api.metadata_ref)(self.unit, id) }
    }
    fn scope_list(&mut self, ids: &[usize]) -> Raw {
        let nodes: Vec<_> = ids.iter().map(|&id| self.md(id)).collect();
        // SAFETY: a uniqued tuple of this unit's nodes.
        unsafe { (self.api.metadata_tuple)(self.unit, nodes.as_ptr(), nodes.len(), 0) }
    }
    fn attribute(&mut self, target: Raw, index: u32, kind: u32, ty: Option<&Type>, n: u64) {
        let ty = ty.map_or(std::ptr::null_mut(), |t| self.ty(t));
        // SAFETY: an attribute of this unit's function or call.
        unsafe { (self.api.attribute)(self.unit, target, index, kind, ty, n) }
    }
    fn extension(&mut self, target: Raw, index: u32, extension: Extension) {
        match extension {
            Extension::None => {}
            Extension::Zero => self.attribute(target, index, ZEXT, None, 0),
            Extension::Sign => self.attribute(target, index, SEXT, None, 0),
        }
    }
    fn signature_attributes(&mut self, target: Raw, sig: &FnSig) {
        self.extension(target, RETURN, sig.extension);
        if sig.nonnull {
            self.attribute(target, RETURN, NONNULL, None, 0);
        }
        for (i, p) in sig.params.iter().enumerate() {
            let index = i as u32 + 1;
            self.extension(target, index, p.extension);
            if p.noalias {
                self.attribute(target, index, NOALIAS, None, 0);
            }
            if p.readonly {
                self.attribute(target, index, READONLY, None, 0);
            }
            if let Some(t) = &p.sret {
                self.attribute(target, index, SRET, Some(t), 0);
            }
            if let Some(t) = &p.byval {
                self.attribute(target, index, BYVAL, Some(t), 0);
            }
            if p.align != 0 {
                self.attribute(target, index, ALIGN, None, u64::from(p.align));
            }
        }
    }
    fn function(&mut self, name: &str, sig: &FnSig, attrs: &FnAttrs) -> Raw {
        let result = self.ty(&sig.result);
        let params: Vec<_> = sig.params.iter().map(|p| self.ty(&p.ty)).collect();
        // SAFETY: a new function of this unit; arrays outlive the call.
        let f = unsafe {
            (self.api.function)(
                self.unit,
                name.as_ptr().cast(),
                name.len(),
                result,
                params.as_ptr(),
                params.len(),
                attrs.linkage as u32,
            )
        };
        self.signature_attributes(f, sig);
        match attrs.inline {
            Inline::Default => {}
            Inline::Always => self.attribute(f, FUNCTION, ALWAYS_INLINE, None, 0),
            Inline::Never => self.attribute(f, FUNCTION, NO_INLINE, None, 0),
        }
        if attrs.nounwind {
            self.attribute(f, FUNCTION, NOUNWIND, None, 0);
        }
        if attrs.argmem_read {
            self.attribute(f, FUNCTION, ARGMEM_READ, None, 0);
        }
        let string = |key: &str, value: &str| {
            // SAFETY: strings are borrowed through the synchronous call.
            unsafe {
                (self.api.string_attribute)(
                    self.unit,
                    f,
                    key.as_ptr().cast(),
                    key.len(),
                    value.as_ptr().cast(),
                    value.len(),
                )
            }
        };
        if let Some(bytes) = attrs.destination_size {
            string("hirsel-destination-size", &bytes.to_string());
        }
        if attrs.view_input {
            string("hirsel-view-input", "");
        }
        if let Some(scope) = attrs.subprogram {
            let scope = self.md(scope);
            // SAFETY: a subprogram node of this unit.
            unsafe { (self.api.subprogram)(f, scope) }
        }
        f
    }
    fn memory(&mut self, instruction: Raw, m: &Memory) {
        if m.invariant {
            let empty = self.md(1);
            // SAFETY: metadata attachment on this unit's instruction.
            unsafe { (self.api.attach)(self.unit, instruction, INVARIANT, empty) }
        }
        self.scopes(instruction, &m.scopes);
    }
    fn scopes(&mut self, instruction: Raw, scopes: &Option<Scoped>) {
        if let Some(s) = scopes {
            let alias = self.scope_list(&s.alias);
            // SAFETY: metadata attachments on this unit's instruction.
            unsafe { (self.api.attach)(self.unit, instruction, ALIAS_SCOPE, alias) };
            if !s.noalias.is_empty() {
                let noalias = self.scope_list(&s.noalias);
                unsafe { (self.api.attach)(self.unit, instruction, NOALIAS_SCOPE, noalias) };
            }
        }
    }
}

impl Sink for NativeSink {
    fn global(&mut self, g: &Global) {
        let ty = self.ty(&g.ty);
        let init = g
            .init
            .as_ref()
            .map_or(std::ptr::null_mut(), |v| self.value(v, &g.ty));
        // SAFETY: the name is borrowed; handles belong to this unit.
        unsafe {
            (self.api.global)(
                self.unit,
                g.name.as_ptr().cast(),
                g.name.len(),
                ty,
                init,
                g.linkage as u32,
                u8::from(g.constant),
                u8::from(g.externally_initialized),
                g.align,
                u8::from(g.absolute_symbol),
            )
        }
    }
    fn declare(&mut self, name: &str, sig: &FnSig, attrs: &FnAttrs) {
        let f = self.function(name, sig, attrs);
        // SAFETY: a function of this unit.
        unsafe { (self.api.finish_declaration)(f) }
    }
    fn module_asm(&mut self, text: &str) {
        // SAFETY: the text is borrowed through the synchronous append.
        unsafe { (self.api.module_asm)(self.unit, text.as_ptr().cast(), text.len()) }
    }
    fn metadata(&mut self, id: usize, node: &Metadata) {
        let (api, u) = (self.api, self.unit);
        // SAFETY (this block): nodes of this unit; strings outlive each call.
        let node = unsafe {
            match node {
                Metadata::Tuple { distinct, items } => {
                    let items: Vec<_> = items
                        .iter()
                        .map(|item| match item {
                            MdItem::Node(id) => self.md(*id),
                            MdItem::String(s) => (api.metadata_string)(u, s.as_ptr().cast(), s.len()),
                            MdItem::I32(n) => (api.metadata_integer)(u, 32, u64::from(*n)),
                        })
                        .collect();
                    (api.metadata_tuple)(u, items.as_ptr(), items.len(), u8::from(*distinct))
                }
                Metadata::File { filename } => (api.di_file)(u, filename.as_ptr().cast(), filename.len()),
                Metadata::CompileUnit { file } => {
                    let file = self.md(*file);
                    (api.di_compile_unit)(u, file)
                }
                Metadata::SubroutineType { types } => {
                    let types = self.md(*types);
                    (api.di_subroutine_type)(u, types)
                }
                Metadata::Subprogram {
                    name,
                    linkage,
                    file,
                    ty,
                    unit,
                } => {
                    let (file, ty, unit) = (self.md(*file), self.md(*ty), self.md(*unit));
                    (api.di_subprogram)(
                        u,
                        name.as_ptr().cast(),
                        name.len(),
                        linkage.as_ptr().cast(),
                        linkage.len(),
                        file,
                        ty,
                        unit,
                    )
                }
                Metadata::Location {
                    line,
                    column,
                    scope,
                } => {
                    let scope = self.md(*scope);
                    (api.di_location)(u, *line as u32, *column, scope)
                }
                Metadata::LexicalBlockFile { scope, file } => {
                    let (scope, file) = (self.md(*scope), self.md(*file));
                    (api.di_lexical_block_file)(u, scope, file)
                }
            }
        };
        // SAFETY: binding replaces this identifier's temporary, if referenced.
        unsafe { (api.metadata_bind)(u, id, node) }
    }
    fn named_metadata(&mut self, name: &str, ids: &[usize]) {
        let nodes: Vec<_> = ids.iter().map(|&id| self.md(id)).collect();
        // SAFETY: name and nodes are borrowed through the synchronous call.
        unsafe {
            (self.api.named_metadata)(
                self.unit,
                name.as_ptr().cast(),
                name.len(),
                nodes.as_ptr(),
                nodes.len(),
            )
        }
    }
    fn begin_function(&mut self, name: &str, sig: &FnSig, params: &[Local], attrs: &FnAttrs) {
        let f = self.function(name, sig, attrs);
        self.locals.clear();
        self.blocks.clear();
        // SAFETY: the new function's body and arguments.
        unsafe { (self.api.begin_body)(self.unit, f) };
        for (i, &local) in params.iter().enumerate() {
            let argument = unsafe { (self.api.parameter)(f, i as u32) };
            self.define(local, argument);
        }
    }
    fn end_function(&mut self) {
        assert!(
            self.pending.is_empty(),
            "undefined locals {:?}",
            self.pending.keys().collect::<Vec<_>>()
        );
        for (label, &block) in &self.blocks {
            // SAFETY: a block of this unit.
            assert!(unsafe { (self.api.block_placed)(block) } != 0, "undefined block {label}");
        }
    }
    fn block(&mut self, label: Label) {
        let block = self.block_ref(label);
        // SAFETY: places this function's block and positions the builder there.
        unsafe { (self.api.begin_block)(self.unit, block) }
    }
    fn alloca(&mut self, name: Local, ty: &Type, align: u32) {
        let t = self.ty(ty);
        // SAFETY: an allocation in this function's entry prefix.
        let v = unsafe { (self.api.alloca)(self.unit, t, align) };
        self.define(name, v);
    }
    fn instruction(&mut self, result: Option<Local>, op: Operation<'_>, location: Option<usize>) {
        let (api, u) = (self.api, self.unit);
        // SAFETY (this block): every operand is a live handle of this unit and
        // every array outlives its synchronous builder call.
        let value = unsafe {
            match op {
                Operation::Binary(op, ty, a, b) => {
                    let (a, b) = (self.value(a, ty), self.value(b, ty));
                    (api.binary)(u, op as u32, a, b)
                }
                Operation::Cast(op, from, v, to) => {
                    let v = self.value(v, from);
                    let to = self.ty(to);
                    (api.cast)(u, op as u32, v, to)
                }
                Operation::Compare(p, ty, a, b) => {
                    let (a, b) = (self.value(a, ty), self.value(b, ty));
                    (api.compare)(u, p as u32, a, b)
                }
                Operation::Select(ty, c, yes, no) => {
                    let c = self.value(c, &Type::I1);
                    let (yes, no) = (self.value(yes, ty), self.value(no, ty));
                    (api.select)(u, c, yes, no)
                }
                Operation::FNeg(ty, v) => {
                    let v = self.value(v, ty);
                    (api.fneg)(u, v)
                }
                Operation::Load(ty, p, m) => {
                    let t = self.ty(ty);
                    let p = self.value(p, &Type::Ptr);
                    let v = (api.load)(u, t, p, m.align, m.ordering.map_or(0, |o| o as u32));
                    self.memory(v, m);
                    v
                }
                Operation::Store(ty, v, p, m) => {
                    let v = self.value(v, ty);
                    let p = self.value(p, &Type::Ptr);
                    let s = (api.store)(u, v, p, m.align, m.ordering.map_or(0, |o| o as u32));
                    self.memory(s, m);
                    s
                }
                Operation::Gep {
                    ty,
                    base,
                    indices,
                    inbounds,
                } => {
                    let t = self.ty(ty);
                    let base = self.value(base, &Type::Ptr);
                    let indices: Vec<_> = indices.iter().map(|(t, v)| self.value(v, t)).collect();
                    (api.gep)(u, t, base, indices.as_ptr(), indices.len(), u8::from(inbounds))
                }
                Operation::Extract(ty, v, index) => {
                    let v = self.value(v, ty);
                    (api.extract)(u, v, index)
                }
                Operation::Insert(ty, aggregate, field, v, index) => {
                    let aggregate = self.value(aggregate, ty);
                    let v = self.value(v, field);
                    (api.insert)(u, aggregate, v, index)
                }
                Operation::Phi(ty, incoming) => {
                    let t = self.ty(ty);
                    let values: Vec<_> = incoming.iter().map(|(v, _)| self.value(v, ty)).collect();
                    let blocks: Vec<_> = incoming.iter().map(|(_, b)| self.block_ref(*b)).collect();
                    (api.phi)(u, t, values.as_ptr(), blocks.as_ptr(), values.len())
                }
                Operation::Call(sig, callee, args, attrs) => {
                    let result = self.ty(&sig.result);
                    let types: Vec<_> = sig.params.iter().map(|p| self.ty(&p.ty)).collect();
                    let callee = self.value(callee, &Type::Ptr);
                    let args: Vec<_> = sig
                        .params
                        .iter()
                        .zip(args)
                        .map(|(p, a)| self.value(a, &p.ty))
                        .collect();
                    let call = (api.call)(
                        u,
                        result,
                        types.as_ptr(),
                        types.len(),
                        callee,
                        args.as_ptr(),
                        args.len(),
                        u8::from(attrs.tail),
                    );
                    self.signature_attributes(call, sig);
                    if attrs.always_inline {
                        self.attribute(call, FUNCTION, ALWAYS_INLINE, None, 0);
                    }
                    if attrs.cold {
                        self.attribute(call, FUNCTION, COLD, None, 0);
                    }
                    self.scopes(call, &attrs.scopes);
                    call
                }
                Operation::CompareExchange(p, ty, expected, new) => {
                    let p = self.value(p, &Type::Ptr);
                    let (expected, new) = (self.value(expected, ty), self.value(new, ty));
                    (api.compare_exchange)(u, p, expected, new)
                }
                Operation::AtomicSub(p, ty, v) => {
                    let p = self.value(p, &Type::Ptr);
                    let v = self.value(v, ty);
                    (api.atomic_sub)(u, p, v)
                }
                Operation::Fence(o) => (api.fence)(u, o as u32),
                Operation::Branch(target) => {
                    let target = self.block_ref(target);
                    (api.branch)(u, target)
                }
                Operation::Conditional(test, yes, no, likely) => {
                    let test = self.value(test, &Type::I1);
                    let (yes, no) = (self.block_ref(yes), self.block_ref(no));
                    let branch = (api.conditional)(u, test, yes, no);
                    if likely {
                        let weights = self.md(0);
                        (api.attach)(u, branch, PROF, weights);
                    }
                    branch
                }
                Operation::Switch(ty, v, default, arms) => {
                    let v = self.value(v, ty);
                    let default = self.block_ref(default);
                    let values: Vec<_> = arms.iter().map(|(n, _)| self.integer(ty, *n)).collect();
                    let blocks: Vec<_> = arms.iter().map(|(_, l)| self.block_ref(*l)).collect();
                    (api.switch_value)(u, v, default, values.as_ptr(), blocks.as_ptr(), values.len())
                }
                Operation::Return(value) => {
                    let value = value.map_or(std::ptr::null_mut(), |(t, v)| self.value(v, t));
                    (api.return_value)(u, value)
                }
                Operation::Unreachable => (api.unreachable)(u),
            }
        };
        if let Some(location) = location {
            let location = self.md(location);
            // SAFETY: a location node of this unit on its instruction.
            unsafe { (api.location)(u, value, location) }
        }
        if let Some(result) = result {
            self.define(result, value);
        }
    }
}
