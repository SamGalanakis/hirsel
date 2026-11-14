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
    scratch: HashMap<(u8, usize), Local>,
    pending_switch: Option<(Type, Value, String, Vec<(u64, String)>)>,
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
fn name_key(name: Local) -> Option<(u8, usize)> {
    match name {
        Local::Temp(i) => Some((0, i)),
        Local::Value(i) => Some((1, i)),
        Local::Address(i) => Some((2, i)),
        Local::Slot(i) => Some((3, i)),
        _ => None,
    }
}
impl<'c> NativeSink<'c> {
    pub fn new(unit: llvm::Unit<'c>) -> Self {
        Self {
            unit,
            types: HashMap::new(),
            blocks: HashMap::new(),
            functions: HashMap::new(),
            globals: HashMap::new(),
            scratch: HashMap::new(),
            pending_switch: None,
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
    pub fn register_static(&mut self, name: &str, bytes: &[u8], align: u32) {
        let i8 = self.unit.integer_type(8);
        let ty = self.unit.array_type(i8, bytes.len() as u64);
        let values: Vec<_> = bytes
            .iter()
            .map(|byte| self.unit.integer(i8, u128::from(*byte)))
            .collect();
        let initializer = self.unit.constant_array(i8, &values);
        let global = self.unit.global(name, ty);
        self.unit
            .initialize_global(global, Some(initializer), 2, true, false, align);
        self.globals.insert(
            name.into(),
            Type::Array(bytes.len() as u64, Box::new(Type::I8)),
        );
    }
    pub fn module_text(&mut self, text: &str) {
        let text = text.trim();
        if !text.starts_with('@') || !text.contains(" = private") {
            return;
        }
        let Some((name, body)) = text.split_once(" = private") else {
            return;
        };
        if !name.starts_with("@hirsel_cause_") || !body.contains("constant") {
            return;
        }
        let numbers = body
            .split("i64 ")
            .skip(1)
            .filter_map(|value| {
                value
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            })
            .collect::<Vec<u64>>();
        if numbers.len() < 2 {
            return;
        }
        let i8 = self.unit.integer_type(8);
        let i64 = self.unit.integer_type(64);
        let bytes = self
            .unit
            .array_type(i8, crate::core::memory_layout::CAUSE_KIND as u64);
        let fields = self.unit.struct_type(&[bytes, i64, i64]);
        let zero_bytes = self.unit.zero(bytes);
        let kind = self.unit.integer(i64, numbers[0] as u128);
        let site = self.unit.integer(i64, numbers[1] as u128);
        let initializer = self.unit.constant_struct(&[zero_bytes, kind, site], false);
        let global_name = name.trim_start_matches('@');
        let global = self.unit.global(global_name, fields);
        self.unit
            .initialize_global(global, Some(initializer), 2, true, false, 8);
        if let Some((prefix, _)) = global_name.rsplit_once('_') {
            for number in numbers {
                let alias = format!("{prefix}_{number}");
                if alias != global_name {
                    let alias_global = self.unit.global(&alias, fields);
                    self.unit
                        .initialize_global(alias_global, Some(initializer), 2, true, false, 8);
                }
            }
        }
    }
    fn cause_global(&mut self, name: &str) -> llvm::Value<'c> {
        let i8 = self.unit.integer_type(8);
        let i64 = self.unit.integer_type(64);
        let bytes = self
            .unit
            .array_type(i8, crate::core::memory_layout::CAUSE_KIND as u64);
        let fields = self.unit.struct_type(&[bytes, i64, i64]);
        let zero_bytes = self.unit.zero(bytes);
        let zero_kind = self.unit.zero(i64);
        let zero_site = self.unit.zero(i64);
        let initializer = self
            .unit
            .constant_struct(&[zero_bytes, zero_kind, zero_site], false);
        let global = self.unit.global(name, fields);
        self.unit
            .initialize_global(global, Some(initializer), 2, true, false, 8);
        global
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
        self.functions.insert(name.into(), sig.clone());
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
            Value::Local(name) => {
                let name = if matches!(ty, Type::Ptr) {
                    name_key(*name)
                        .and_then(|key| self.scratch.get(&key).copied())
                        .unwrap_or(*name)
                } else {
                    *name
                };
                self.unit.local(raw, local(name))
            }
            Value::Integer(bits) => self.unit.integer(raw, *bits),
            Value::Boolean(b) => self.unit.integer(raw, u128::from(*b)),
            Value::Null | Value::Zero => self.unit.zero(raw),
            Value::Poison => self.unit.poison(raw),
            Value::Global(name) => {
                if name.starts_with("hirsel_cause_") {
                    return self.cause_global(name);
                }
                if let Some(sig) = self.functions.get(name).cloned() {
                    self.function(name, &sig)
                } else {
                    let t = self.globals.get(name).cloned().unwrap_or(Type::Ptr);
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

    fn raw_type(text: &str) -> Type {
        let text = text
            .trim()
            .trim_end_matches(" zeroext")
            .trim_end_matches(" signext");
        match text {
            "void" => Type::Void,
            "ptr" => Type::Ptr,
            "float" => Type::Float,
            "double" => Type::Double,
            _ => text
                .strip_prefix('i')
                .and_then(|bits| bits.parse().ok())
                .map_or(Type::Ptr, Type::Int),
        }
    }
    fn raw_local(text: &str) -> Option<Local> {
        let text = text.trim().strip_prefix('%')?;
        match text {
            "context" => Some(Local::Context),
            "dest" => Some(Local::Destination),
            "caller_site" => Some(Local::CallerSite),
            "result" => Some(Local::Result),
            "sret" => Some(Local::Sret),
            "payload" => Some(Local::Payload),
            "input" => Some(Local::Input),
            "worklist" => Some(Local::Worklist),
            _ => {
                let (kind, index) = text.split_at(1);
                let index = index.parse().ok()?;
                Some(match kind {
                    "t" => Local::Temp(index),
                    "v" => Local::Value(index),
                    "a" => Local::Address(index),
                    "s" => Local::Slot(index),
                    "p" => Local::Param(index),
                    _ => return None,
                })
            }
        }
    }
    fn raw_value(text: &str) -> Value {
        let text = text.trim().trim_end_matches(',');
        if let Some(local) = Self::raw_local(text) {
            return Value::Local(local);
        }
        match text {
            "true" => Value::Boolean(true),
            "false" => Value::Boolean(false),
            "null" => Value::Null,
            "poison" => Value::Poison,
            "zeroinitializer" => Value::Zero,
            _ if text.starts_with('@') => Value::Global(text[1..].to_owned()),
            _ => text.parse::<i128>().map_or_else(
                |_| Value::Global(text.to_owned()),
                |n| Value::Integer(n as u128),
            ),
        }
    }
    fn raw_result(text: &str) -> Option<Local> {
        text.split_once(" = ")
            .and_then(|(name, _)| Self::raw_local(name))
    }
    fn raw_body<'a>(text: &'a str) -> &'a str {
        text.split_once(" = ").map_or(text, |(_, body)| body)
    }
}
impl Sink for NativeSink<'_> {
    fn raw_alloca(&mut self, text: &str) {
        let text = text.trim();
        let (name, body) = text
            .split_once(" = alloca ")
            .unwrap_or_else(|| panic!("native sink raw alloca unsupported: {text}"));
        let local = Self::raw_local(name)
            .unwrap_or_else(|| panic!("native sink raw alloca name unsupported: {name}"));
        let mut parts = body.split(", align ");
        let ty = Self::raw_type(parts.next().unwrap());
        let align = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        self.alloca(local, &ty, align);
    }
    fn raw(&mut self, text: &str, location: Option<usize>) {
        let text = text.trim();
        if let Some((ty, value, default, arms)) = self.pending_switch.as_mut() {
            if text == "]" {
                let ty = ty.clone();
                let value = value.clone();
                let default = default.clone();
                let arms = std::mem::take(arms);
                self.pending_switch = None;
                self.instruction(
                    None,
                    Operation::Switch(ty, &value, &default, &arms),
                    location,
                );
                return;
            }
            if let Some((constant, block)) = text.split_once(", label %") {
                let constant = constant
                    .split_whitespace()
                    .last()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                arms.push((constant, block.to_owned()));
                return;
            }
        }
        let result = Self::raw_result(text);
        let body = Self::raw_body(text);
        if let Some(rest) = body.strip_prefix("select i1 ") {
            let parts: Vec<_> = rest.split(", ").collect();
            if parts.len() == 3 {
                let (ty, yes) = parts[1].split_once(' ').unwrap();
                let (_, no) = parts[2].split_once(' ').unwrap();
                let condition = Self::raw_value(parts[0]);
                let yes = Self::raw_value(yes);
                let no = Self::raw_value(no);
                self.instruction(
                    result,
                    Operation::Select(Self::raw_type(ty), &condition, &yes, &no),
                    location,
                );
                return;
            }
        }
        if let Some(rest) = body.strip_prefix("load ") {
            let (ty, pointer) = rest
                .split_once(", ptr ")
                .unwrap_or_else(|| panic!("native sink raw load unsupported: {text}"));
            let pointer = pointer
                .split_once(", align ")
                .map_or(pointer, |(value, _)| value);
            let pointer = Self::raw_value(pointer);
            self.instruction(
                result,
                Operation::Load(Self::raw_type(ty), &pointer, &Memory::default()),
                location,
            );
            return;
        }
        if let Some(rest) = body.strip_prefix("getelementptr ") {
            let parts: Vec<_> = rest.split(", ").collect();
            if parts.len() >= 2 {
                let element = Self::raw_type(parts[0]);
                let (_, pointer) = parts[1].split_once(' ').unwrap();
                let pointer = Self::raw_value(pointer);
                let indices = parts[2..]
                    .iter()
                    .map(|part| {
                        let (ty, value) = part.split_once(' ').unwrap();
                        (Self::raw_type(ty), Self::raw_value(value))
                    })
                    .collect::<Vec<_>>();
                self.instruction(
                    result,
                    Operation::Gep(element, &pointer, &indices),
                    location,
                );
                return;
            }
        }
        if let Some(rest) = body.strip_prefix("phi ") {
            let (ty, incoming) = rest.split_once(' ').unwrap();
            let incoming = incoming
                .split("], [")
                .map(|part| part.trim_matches(['[', ']']))
                .map(|part| {
                    let (value, block) = part.split_once(", %").unwrap();
                    (Self::raw_value(value), block.to_owned())
                })
                .collect::<Vec<_>>();
            self.instruction(
                result,
                Operation::Phi(Self::raw_type(ty), &incoming),
                location,
            );
            return;
        }
        if let Some(rest) = body.strip_prefix("extractvalue ") {
            let (ty, rest) = rest.split_once(' ').unwrap();
            let (value, index) = rest.rsplit_once(", ").unwrap();
            let index = index.parse().unwrap_or(0);
            self.instruction(
                result,
                Operation::Extract(Self::raw_type(ty), &Self::raw_value(value), index),
                location,
            );
            return;
        }
        if let Some(rest) = body.strip_prefix("insertvalue ") {
            let (ty, rest) = rest.split_once(' ').unwrap();
            let (aggregate, rest) = rest.split_once(", ").unwrap();
            let (field, index) = rest.rsplit_once(", ").unwrap();
            let (field_ty, field) = field.split_once(' ').unwrap();
            let index = index.parse().unwrap_or(0);
            let aggregate = Self::raw_value(aggregate);
            let field = Self::raw_value(field);
            self.instruction(
                result,
                Operation::Insert(
                    Self::raw_type(ty),
                    &aggregate,
                    Self::raw_type(field_ty),
                    &field,
                    index,
                ),
                location,
            );
            return;
        }
        if let Some(value) = body.strip_prefix("fneg ") {
            let (ty, value) = value.split_once(' ').unwrap();
            self.instruction(
                result,
                Operation::FNeg(Self::raw_type(ty), &Self::raw_value(value)),
                location,
            );
            return;
        }
        for (name, op) in [
            ("add", Binary::Add),
            ("sub", Binary::Sub),
            ("mul", Binary::Mul),
            ("udiv", Binary::UDiv),
            ("sdiv", Binary::SDiv),
            ("and", Binary::And),
            ("or", Binary::Or),
            ("xor", Binary::Xor),
        ] {
            if let Some(rest) = body.strip_prefix(&format!("{name} ")) {
                let (ty, values) = rest.split_once(' ').unwrap();
                let parts: Vec<_> = values.split(", ").collect();
                if parts.len() == 2 {
                    let left = Self::raw_value(parts[0]);
                    let right = Self::raw_value(parts[1]);
                    self.instruction(
                        result,
                        Operation::Binary(op, Self::raw_type(ty), &left, &right),
                        location,
                    );
                    return;
                }
            }
        }
        for (name, op) in [
            ("eq", Predicate::Eq),
            ("ne", Predicate::Ne),
            ("ult", Predicate::Ult),
            ("ule", Predicate::Ule),
            ("ugt", Predicate::Ugt),
            ("uge", Predicate::Uge),
            ("slt", Predicate::Slt),
            ("sle", Predicate::Sle),
            ("sgt", Predicate::Sgt),
            ("sge", Predicate::Sge),
        ] {
            for prefix in ["icmp", "fcmp"] {
                let marker = format!("{prefix} {name} ");
                if let Some(rest) = body.strip_prefix(&marker) {
                    let (ty, values) = rest.split_once(' ').unwrap();
                    let parts: Vec<_> = values.split(", ").collect();
                    if parts.len() == 2 {
                        let left = Self::raw_value(parts[0]);
                        let right = Self::raw_value(parts[1]);
                        self.instruction(
                            result,
                            Operation::Compare(op, Self::raw_type(ty), &left, &right),
                            location,
                        );
                        return;
                    }
                }
            }
        }
        for (name, op) in [
            ("zext", Cast::ZExt),
            ("sext", Cast::SExt),
            ("trunc", Cast::Trunc),
            ("bitcast", Cast::Bitcast),
            ("ptrtoint", Cast::PtrToInt),
            ("inttoptr", Cast::IntToPtr),
        ] {
            let marker = format!("{name} ");
            if let Some(rest) = body.strip_prefix(&marker) {
                if let Some((source, to)) = rest.split_once(" to ") {
                    let (from, value) = source.split_once(' ').unwrap();
                    let value = Self::raw_value(value);
                    self.instruction(
                        result,
                        Operation::Cast(op, Self::raw_type(from), &value, Self::raw_type(to)),
                        location,
                    );
                    return;
                }
            }
        }
        if let Some(rest) = body
            .strip_prefix("call ")
            .or_else(|| body.strip_prefix("tail call "))
        {
            let is_cold = rest.ends_with(" cold");
            let rest = rest.trim_end_matches(" cold");
            let (result_ty, callee_and_args) = rest.split_once(' ').unwrap();
            let at = callee_and_args.find('@').unwrap();
            let open = callee_and_args[at..].find('(').unwrap() + at;
            let name = &callee_and_args[at + 1..open];
            let args_text = callee_and_args[open + 1..].trim_end_matches(')');
            let mut params = Vec::new();
            let mut args = Vec::new();
            if !args_text.is_empty() {
                for argument in args_text.split(", ") {
                    let (ty, value) = argument.rsplit_once(' ').unwrap();
                    params.push(Parameter::plain(Self::raw_type(ty)));
                    args.push(Self::raw_value(value));
                }
            }
            let signature = Signature {
                result: Self::raw_type(result_ty),
                extension: Extension::None,
                params,
            };
            let callee = Value::Global(name.to_owned());
            let attrs = CallAttrs {
                cold: is_cold,
                ..Default::default()
            };
            self.instruction(
                result,
                Operation::Call(&signature, &callee, &args, &attrs),
                location,
            );
            return;
        }
        if let Some(rest) = text.strip_prefix("switch ") {
            let head = rest.split_once(" [").map_or(rest, |(head, _)| head);
            let (ty, rest) = head.split_once(' ').unwrap();
            let (value, default) = rest.split_once(", label %").unwrap();
            self.pending_switch = Some((
                Self::raw_type(ty),
                Self::raw_value(value),
                default.to_owned(),
                Vec::new(),
            ));
            return;
        }
        if text.ends_with(':') {
            self.block(text.trim_end_matches(':'));
            return;
        }
        if text == "unreachable" {
            self.instruction(None, Operation::Unreachable, location);
            return;
        }
        if let Some(target) = text.strip_prefix("br label %") {
            self.instruction(None, Operation::Branch(target), location);
            return;
        }
        if let Some(rest) = text.strip_prefix("br i1 ") {
            let parts: Vec<_> = rest.split(", label %").collect();
            if parts.len() == 3 {
                let condition = Self::raw_value(parts[0]);
                self.instruction(
                    None,
                    Operation::Conditional(&condition, parts[1], parts[2], false),
                    location,
                );
                return;
            }
        }
        if let Some(rest) = text.strip_prefix("ret ") {
            if rest == "void" {
                self.instruction(None, Operation::Return(Type::Void, None), location);
                return;
            }
            let (ty, value) = rest
                .split_once(' ')
                .unwrap_or_else(|| panic!("native sink raw return unsupported: {text}"));
            let ty = Self::raw_type(ty);
            let value = Self::raw_value(value);
            self.instruction(None, Operation::Return(ty, Some(&value)), location);
            return;
        }
        if let Some(rest) = text.strip_prefix("store ") {
            let (left, right) = rest
                .split_once(", ptr ")
                .unwrap_or_else(|| panic!("native sink raw store unsupported: {text}"));
            let (ty, value) = left
                .split_once(' ')
                .unwrap_or_else(|| panic!("native sink raw store type unsupported: {text}"));
            let align = right
                .split_once(", align ")
                .and_then(|(_, value)| value.parse().ok())
                .unwrap_or(0);
            let pointer = right
                .split_once(", align ")
                .map_or(right, |(value, _)| value);
            let ty = Self::raw_type(ty);
            let value = Self::raw_value(value);
            let pointer = Self::raw_value(pointer);
            let memory = Memory {
                align,
                ..Default::default()
            };
            self.instruction(
                None,
                Operation::Store(ty, &value, &pointer, &memory),
                location,
            );
            return;
        }
        panic!("native sink raw operation unsupported: {text}");
    }
    fn alloca(&mut self, name: Local, ty: &Type, align: u32) {
        let ty = self.ty(ty);
        let actual = match name_key(name) {
            Some((kind, i)) => {
                let slot = Local::Slot(usize::MAX - (i.wrapping_mul(4) + usize::from(kind)));
                self.scratch.insert((kind, i), slot);
                slot
            }
            None => name,
        };
        self.unit.alloca(ty, align, local(actual));
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
