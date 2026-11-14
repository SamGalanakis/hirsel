//! Function-local call references and reusable ABI buffers. Runtime declarations
//! are shared by the module, so lowering a call creates no temporary signature.
use super::*;
use cranelift_codegen::ir::{
    ArgumentPurpose, BlockArg, FuncRef, SigRef, StackSlot, StackSlotData, StackSlotKind, Value,
};
use std::{cell::RefCell, collections::HashMap, ops::Index};

pub(super) struct Imported {
    signature: abi::Signature,
    function: cranelift_module::FuncId,
}
#[derive(Default)]
pub(super) struct Imports {
    functions: HashMap<&'static str, Imported>,
    raise: Option<cranelift_module::FuncId>,
}

pub(super) struct Calls<'a> {
    imports: &'a RefCell<Imports>,
    references: HashMap<cranelift_module::FuncId, FuncRef>,
    indirect: HashMap<usize, SigRef>,
    pub args: Vec<Value>,
    pub results: Vec<Value>,
    pub edges: [Vec<BlockArg>; 2],
    slot: Option<StackSlot>,
    by_value: Option<StackSlot>,
}
impl<'a> Calls<'a> {
    pub fn new(
        imports: &'a RefCell<Imports>,
        signatures: &[abi::Signature],
        edge_arity: usize,
    ) -> Self {
        let args = signatures
            .iter()
            .map(|s| s.clif.params.len())
            .max()
            .unwrap_or(0)
            .max(8);
        let results = signatures
            .iter()
            .map(|s| s.results.len())
            .max()
            .unwrap_or(0);
        Self {
            imports,
            references: HashMap::new(),
            indirect: HashMap::new(),
            args: Vec::with_capacity(args),
            results: Vec::with_capacity(results),
            edges: std::array::from_fn(|_| Vec::with_capacity(edge_arity)),
            slot: None,
            by_value: None,
        }
    }
    pub fn reference(
        &mut self,
        object: &mut ObjectModule,
        b: &mut FunctionBuilder<'_>,
        function: cranelift_module::FuncId,
    ) -> FuncRef {
        *self
            .references
            .entry(function)
            .or_insert_with(|| object.declare_func_in_func(function, b.func))
    }
    pub fn indirect(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        key: usize,
        signature: &ir::Signature,
    ) -> SigRef {
        *self
            .indirect
            .entry(key)
            .or_insert_with(|| b.import_signature(signature.clone()))
    }
    pub fn own_fn_cleanup(&mut self, object: &ObjectModule, b: &mut FunctionBuilder<'_>) -> SigRef {
        *self.indirect.entry(usize::MAX).or_insert_with(|| {
            b.import_signature(
                abi::Signature::c(object, &[Ty::Ptr, Ty::Ptr], &[Ty::Scalar(Scalar::Bool)]).clif,
            )
        })
    }
    pub fn storage(&mut self, b: &mut FunctionBuilder<'_>, size: u32) -> Value {
        let size = size.max(8).next_multiple_of(8);
        let slot = if let Some(slot) = self.slot {
            b.func.sized_stack_slots[slot].size = b.func.sized_stack_slots[slot].size.max(size);
            slot
        } else {
            let slot =
                b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
            self.slot = Some(slot);
            slot
        };
        b.ins().stack_addr(types::I64, slot, 0)
    }
    pub fn c(
        &mut self,
        object: &mut ObjectModule,
        b: &mut FunctionBuilder<'_>,
        name: &'static str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<NativeValues> {
        let arm = abi::aarch64(object);
        let structure = match name {
            "hirsel_core_spawn" => Some((4, 64)),
            "hirsel_core_channel_new" => Some((2, 32)),
            _ => None,
        };
        let imports = self.imports;
        let mut imports = imports.borrow_mut();
        if !imports.functions.contains_key(name) {
            let mut signature = abi::Signature::c(object, params, results);
            if let Some((index, bytes)) = structure.filter(|_| !arm) {
                signature.clif.params[index + usize::from(signature.indirect)] =
                    AbiParam::special(types::I64, ArgumentPurpose::StructArgument(bytes));
            }
            let function = object
                .declare_function(name, Linkage::Import, &signature.clif)
                .map_err(error)?;
            imports.functions.insert(
                name,
                Imported {
                    signature,
                    function,
                },
            );
        }
        let imported = &imports.functions[name];
        let signature = &imported.signature;
        let storage = signature.indirect.then(|| self.storage(b, signature.size));
        let mut actual = std::mem::take(&mut self.args);
        actual.clear();
        if let Some(storage) = storage {
            actual.push(storage);
        }
        actual.extend_from_slice(args);
        if let Some((index, bytes)) = structure.filter(|_| arm) {
            // AAPCS64 passes a caller-owned copy by pointer. The descriptor
            // source may share ABI scratch with the result, so keep this copy
            // separate from the return buffer and from its original storage.
            let slot = if let Some(slot) = self.by_value {
                b.func.sized_stack_slots[slot].size =
                    b.func.sized_stack_slots[slot].size.max(bytes);
                slot
            } else {
                let slot = b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    bytes,
                    3,
                ));
                self.by_value = Some(slot);
                slot
            };
            let copy = b.ins().stack_addr(types::I64, slot, 0);
            b.emit_small_memory_copy(
                object.target_config(),
                copy,
                args[index],
                u64::from(bytes),
                8,
                8,
                true,
                ir::MemFlagsData::trusted(),
            );
            actual[index + usize::from(signature.indirect)] = copy;
        }
        let function = self.reference(object, b, imported.function);
        let inst = b.ins().call(function, &actual);
        self.args = actual;
        let mut returned = [Value::from_u32(0); 4];
        let len = b.inst_results(inst).len();
        returned[..len].copy_from_slice(b.inst_results(inst));
        let mut out = NativeValues {
            values: [Value::from_u32(0); 8],
            len: 0,
        };
        signature.unpack(b, &returned[..len], storage, |value| {
            out.values[out.len] = value;
            out.len += 1;
        });
        Ok(out)
    }
    pub fn raise(
        &mut self,
        object: &mut ObjectModule,
        b: &mut FunctionBuilder<'_>,
        context: Value,
        cause: Value,
    ) -> Result<()> {
        let imports = self.imports;
        let mut imports = imports.borrow_mut();
        let function = if let Some(function) = imports.raise {
            function
        } else {
            let mut signature = object.make_signature();
            signature.params.push(AbiParam::new(types::I64));
            signature.params.push(if abi::aarch64(object) {
                AbiParam::new(types::I64)
            } else {
                AbiParam::special(
                    types::I64,
                    ArgumentPurpose::StructArgument(super::memory_layout::CAUSE_BYTES as u32),
                )
            });
            let function = object
                .declare_function("hirsel_core_raise", Linkage::Import, &signature)
                .map_err(error)?;
            imports.raise = Some(function);
            function
        };
        let reference = self.reference(object, b, function);
        b.ins().call(reference, &[context, cause]);
        Ok(())
    }
}
// Runtime rows have a small, frozen result list. Internal function rows use the
// reusable dynamically sized buffer above and have no source-level arity cap.
pub(super) struct NativeValues {
    values: [Value; 8],
    len: usize,
}
impl Index<usize> for NativeValues {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        &self.values[index]
    }
}
