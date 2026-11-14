//! Streaming NoFolder sink. Values are typed local identities or constants;
//! resolving a forward local always uses the builder's current binding.
use super::*;
use crate::construction as llvm;
use std::collections::HashMap;

pub(crate) struct NativeSink<'c> {
    pub unit: llvm::Unit<'c>,
    types: HashMap<Type, llvm::Type<'c>>,
    blocks: HashMap<String, llvm::Block<'c>>,
    functions: HashMap<String, Signature>,
    globals: HashMap<String, Type>,
}
fn local(name: Local) -> llvm::Local {
    let (kind, index) = match name {
        Local::Temp(i) => (0, i),
        Local::Value(i) => (1, i),
        Local::Address(i) => (2, i),
        Local::Slot(i) => (3, i),
        Local::Param(i) => (4, i),
        Local::Context => (5, 0),
        Local::Destination => (6, 0),
        Local::CallerSite => (7, 0),
        Local::Result => (8, 0),
        Local::Sret => (9, 0),
        Local::Payload => (10, 0),
        Local::Input => (11, 0),
        Local::Worklist => (12, 0),
    };
    llvm::Local { kind, index }
}
impl<'c> NativeSink<'c> {
    pub fn new(unit: llvm::Unit<'c>) -> Self {
        Self {
            unit,
            types: HashMap::new(),
            blocks: HashMap::new(),
            functions: HashMap::new(),
            globals: HashMap::new(),
        }
    }
    pub fn print(&self) -> Result<String, String> {
        self.unit.print()
    }
    pub fn bitcode(&self) -> Result<Vec<u8>, String> {
        self.unit.bitcode()
    }
    pub fn take(
        self,
        context: std::rc::Rc<crate::construction::Context>,
    ) -> crate::construction::NativeModule {
        crate::construction::NativeModule::new(context, self.unit.take())
    }
    fn ty(&mut self, ty: &Type) -> llvm::Type<'c> {
        if let Some(&t) = self.types.get(ty) {
            return t;
        }
        let t = match ty {
            Type::Void => self.unit.void_type(),
            Type::Int(bits) => self.unit.integer_type(*bits),
            Type::Float => self.unit.float_type(32),
            Type::Double => self.unit.float_type(64),
            Type::Ptr => self.unit.pointer_type(),
            Type::Array(count, element) => {
                let element = self.ty(element);
                self.unit.array_type(element, *count)
            }
            Type::Vector(count, element) => {
                let element = self.ty(element);
                self.unit.vector_type(element, *count)
            }
            Type::Struct(fields, packed) => {
                let fields: Vec<_> = fields.iter().map(|f| self.ty(f)).collect();
                if *packed {
                    self.unit.packed_struct_type(&fields)
                } else {
                    self.unit.struct_type(&fields)
                }
            }
        };
        self.types.insert(ty.clone(), t);
        t
    }
    pub fn register_function(&mut self, name: &str, signature: &Signature) {
        self.functions.insert(name.into(), signature.clone());
    }
    pub fn register_global(&mut self, name: &str, ty: &Type) {
        self.globals.insert(name.into(), ty.clone());
    }
    fn function(&mut self, name: &str, sig: &Signature) -> llvm::Value<'c> {
        let result = self.ty(&sig.result);
        let params: Vec<_> = sig.params.iter().map(|p| self.ty(&p.ty)).collect();
        self.unit.function(name, result, &params)
    }
    pub fn begin_function(
        &mut self,
        name: &str,
        sig: &Signature,
        params: &[Local],
        internal: bool,
    ) {
        let f = self.function(name, sig);
        self.signature_attributes(f, sig);
        self.unit.define(f, internal);
        self.blocks.clear();
        for (i, &name) in params.iter().enumerate() {
            let p = self.unit.parameter(f, i as u32, local(name));
            self.unit.bind(local(name), p);
        }
    }
    fn value(&mut self, value: &Value, ty: &Type) -> llvm::Value<'c> {
        let raw = self.ty(ty);
        match value {
            Value::Local(name) => self.unit.local(raw, local(*name)),
            Value::Integer(bits) => self.unit.integer(raw, *bits),
            Value::Boolean(b) => self.unit.integer(raw, u128::from(*b)),
            Value::Null | Value::Zero => self.unit.zero(raw),
            Value::Poison => self.unit.poison(raw),
            Value::Global(name) => {
                if let Some(sig) = self.functions.get(name).cloned() {
                    self.function(name, &sig)
                } else {
                    let t = self.globals[name].clone();
                    let t = self.ty(&t);
                    self.unit.global(name, t)
                }
            }
            Value::Bitcast { from, bits, to } => {
                let from = self.ty(from);
                let to = self.ty(to);
                let v = self.unit.integer(from, *bits);
                self.unit.constant_cast(Cast::Bitcast as u32, v, to)
            }
            Value::IntToPtr(bits) => {
                let int = self.ty(&Type::I64);
                let v = self.unit.integer(int, u128::from(*bits));
                self.unit.constant_cast(Cast::IntToPtr as u32, v, raw)
            }
            Value::PtrToInt(name) => {
                let v = self.value(&Value::Global(name.clone()), &Type::Ptr);
                self.unit.constant_cast(Cast::PtrToInt as u32, v, raw)
            }
            Value::Add(left, right) => {
                let left = self.value(left, ty);
                let right = self.unit.integer(raw, u128::from(*right));
                self.unit.constant_add(left, right)
            }
        }
    }
    fn block_ref(&mut self, name: &str) -> llvm::Block<'c> {
        *self
            .blocks
            .entry(name.into())
            .or_insert_with(|| self.unit.block(name))
    }
    fn extension(&mut self, value: llvm::Value<'c>, index: u32, extension: Extension) {
        match extension {
            Extension::None => {}
            Extension::Zero => self.unit.attribute(value, index, 0, None),
            Extension::Sign => self.unit.attribute(value, index, 1, None),
        }
    }
    fn signature_attributes(&mut self, value: llvm::Value<'c>, sig: &Signature) {
        self.extension(value, 0, sig.extension);
        for (i, p) in sig.params.iter().enumerate() {
            let index = i as u32 + 1;
            self.extension(value, index, p.extension);
            if p.noalias {
                self.unit.attribute(value, index, 2, None);
            }
            if p.readonly {
                self.unit.attribute(value, index, 3, None);
            }
            if let Some(t) = &p.sret {
                let t = self.ty(t);
                self.unit.attribute(value, index, 4, Some(t));
            }
            if let Some(t) = &p.byval {
                let t = self.ty(t);
                self.unit.attribute(value, index, 5, Some(t));
            }
        }
    }
    fn scopes(&mut self, value: llvm::Value<'c>, name: &str, ids: &[usize]) {
        if ids.is_empty() {
            return;
        }
        let refs: Vec<_> = ids.iter().map(|i| self.unit.metadata_ref(*i)).collect();
        let tuple = self.unit.metadata_tuple(&refs, false);
        self.unit.attach(value, name, tuple);
    }
    fn memory(&mut self, value: llvm::Value<'c>, memory: &Memory) {
        if memory.invariant {
            let empty = self.unit.metadata_tuple(&[], false);
            self.unit.attach(value, "invariant.load", empty);
        }
        self.scopes(value, "alias.scope", &memory.alias_scopes);
        self.scopes(value, "noalias", &memory.noalias_scopes);
    }
}
impl Sink for NativeSink<'_> {
    fn alloca(&mut self, name: Local, ty: &Type, align: u32) {
        let ty = self.ty(ty);
        self.unit.alloca(ty, align, local(name));
    }
    fn block(&mut self, name: &str) {
        let block = self.block_ref(name);
        self.unit.begin_block(block);
    }
    fn instruction(&mut self, result: Option<Local>, op: Operation<'_>, location: Option<usize>) {
        let location = location.map(|id| self.unit.metadata_ref(id));
        self.unit.location(location);
        let v = match op {
            Operation::Binary(op, ty, a, b) => {
                let a = self.value(a, &ty);
                let b = self.value(b, &ty);
                Some(self.unit.binary(op as u32, a, b))
            }
            Operation::Cast(op, from, v, to) => {
                let v = self.value(v, &from);
                let to = self.ty(&to);
                Some(self.unit.cast(op as u32, v, to))
            }
            Operation::Compare(op, ty, a, b) => {
                let a = self.value(a, &ty);
                let b = self.value(b, &ty);
                Some(self.unit.compare(op as u32, a, b))
            }
            Operation::Select(ty, c, a, b) => {
                let c = self.value(c, &Type::I1);
                let a = self.value(a, &ty);
                let b = self.value(b, &ty);
                Some(self.unit.select(c, a, b))
            }
            Operation::FNeg(ty, v) => {
                let v = self.value(v, &ty);
                Some(self.unit.fneg(v))
            }
            Operation::Load(ty, p, m) => {
                let ty = self.ty(&ty);
                let p = self.value(p, &Type::Ptr);
                let v = self.unit.load(ty, p, m.align);
                self.memory(v, m);
                Some(v)
            }
            Operation::Store(ty, v, p, m) => {
                let v = self.value(v, &ty);
                let p = self.value(p, &Type::Ptr);
                let store = self.unit.store(v, p, m.align);
                self.memory(store, m);
                None
            }
            Operation::Gep(ty, p, indices) => {
                let ty = self.ty(&ty);
                let p = self.value(p, &Type::Ptr);
                let indices: Vec<_> = indices.iter().map(|(t, v)| self.value(v, t)).collect();
                Some(self.unit.gep(ty, p, &indices))
            }
            Operation::Extract(ty, v, index) => {
                let v = self.value(v, &ty);
                Some(self.unit.extract(v, index))
            }
            Operation::Insert(ty, a, t, v, index) => {
                let a = self.value(a, &ty);
                let v = self.value(v, &t);
                Some(self.unit.insert(a, v, index))
            }
            Operation::Phi(ty, incoming) => {
                let t = self.ty(&ty);
                let values: Vec<_> = incoming.iter().map(|(v, _)| self.value(v, &ty)).collect();
                let blocks: Vec<_> = incoming.iter().map(|(_, b)| self.block_ref(b)).collect();
                Some(self.unit.phi(t, &values, &blocks))
            }
            Operation::Call(sig, callee, args, attrs) => {
                let callee = if let Value::Global(name) = callee {
                    self.function(name, sig)
                } else {
                    self.value(callee, &Type::Ptr)
                };
                let result = self.ty(&sig.result);
                let params: Vec<_> = sig.params.iter().map(|p| self.ty(&p.ty)).collect();
                let args: Vec<_> = args
                    .iter()
                    .zip(&sig.params)
                    .map(|(v, p)| self.value(v, &p.ty))
                    .collect();
                let call = self.unit.call(result, &params, callee, &args);
                self.signature_attributes(call, sig);
                if attrs.cold {
                    self.unit.attribute(call, u32::MAX, 8, None);
                }
                if attrs.tail {
                    self.unit.tail(call);
                }
                self.scopes(call, "alias.scope", &attrs.alias_scopes);
                self.scopes(call, "noalias", &attrs.noalias_scopes);
                Some(call)
            }
            Operation::Branch(target) => {
                let target = self.block_ref(target);
                self.unit.branch(target);
                None
            }
            Operation::Conditional(c, a, b, likely) => {
                let c = self.value(c, &Type::I1);
                let a = self.block_ref(a);
                let b = self.block_ref(b);
                let branch = self.unit.conditional(c, a, b);
                if likely {
                    let tag = self.unit.metadata_string("branch_weights");
                    let yes = self.unit.metadata_integer(32, 2000);
                    let no = self.unit.metadata_integer(32, 1);
                    let tuple = self.unit.metadata_tuple(&[tag, yes, no], false);
                    self.unit.attach(branch, "prof", tuple);
                }
                None
            }
            Operation::Switch(ty, value, default, arms) => {
                let value = self.value(value, &ty);
                let default = self.block_ref(default);
                let values: Vec<_> = arms
                    .iter()
                    .map(|(v, _)| self.value(&Value::from(*v), &ty))
                    .collect();
                let blocks: Vec<_> = arms.iter().map(|(_, b)| self.block_ref(b)).collect();
                self.unit.switch_value(value, default, &values, &blocks);
                None
            }
            Operation::Return(ty, value) => {
                let value = value.map(|v| self.value(v, &ty));
                self.unit.return_value(value);
                None
            }
            Operation::Unreachable => {
                self.unit.unreachable();
                None
            }
        };
        if let Some(name) = result {
            self.unit.bind(local(name), v.unwrap());
        }
    }
}
