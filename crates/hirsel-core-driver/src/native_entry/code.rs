//! Small C-ABI functions; no language lowering or alternate program emitter.
use super::*;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData as MemFlags, Value, condcodes::IntCC};
pub(super) use ir::types::{I8, I32, I64};

pub(super) struct Code<'a, 'b> {
    pub object: &'a mut ObjectModule,
    pub b: FunctionBuilder<'b>,
}
impl Code<'_, '_> {
    pub fn constant(&mut self, n: u64) -> Value {
        self.b.ins().iconst(I64, n as i64)
    }
    pub fn address(&mut self, id: DataId) -> Value {
        let global = self.object.declare_data_in_func(id, self.b.func);
        self.b.ins().symbol_value(I64, global)
    }
    pub fn load(&mut self, ty: ir::Type, address: Value, offset: i32) -> Value {
        self.b
            .ins()
            .load(ty, MemFlags::new().with_notrap(), address, offset)
    }
    pub fn store(&mut self, value: Value, address: Value, offset: i32) {
        self.b
            .ins()
            .store(MemFlags::new().with_notrap(), value, address, offset);
    }
    pub fn stack(&mut self, size: u32, align: u8) -> Value {
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            size,
            align,
        ));
        self.b.ins().stack_addr(I64, slot, 0)
    }
    fn call(
        &mut self,
        name: &str,
        result: Option<ir::Type>,
        args: &[Value],
    ) -> Result<Option<Value>> {
        let types: Vec<_> = args
            .iter()
            .map(|&v| self.b.func.dfg.value_type(v))
            .collect();
        let signature = signature(self.object, &types, result);
        let id = self
            .object
            .declare_function(name, Linkage::Import, &signature)
            .map_err(error)?;
        let reference = self.object.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(reference, args);
        Ok(result.map(|_| self.b.inst_results(call)[0]))
    }
    pub fn value_call(&mut self, name: &str, ty: ir::Type, args: &[Value]) -> Result<Value> {
        Ok(self.call(name, Some(ty), args)?.expect("valued C call"))
    }
    pub fn void_call(&mut self, name: &str, args: &[Value]) -> Result<()> {
        self.call(name, None, args)?;
        Ok(())
    }
    pub fn string(&mut self, bytes: &[u8]) -> Result<Value> {
        let mut bytes = bytes.to_vec();
        bytes.push(0);
        let id = data::bytes(self.object, None, bytes, 1)?;
        Ok(self.address(id))
    }
    pub fn return_i32(&mut self, status: i64) {
        let value = self.b.ins().iconst(I32, status);
        self.b.ins().return_(&[value]);
    }
    pub fn if_nonzero(&mut self, value: Value) -> (ir::Block, ir::Block) {
        let yes = self.b.create_block();
        let no = self.b.create_block();
        self.b.ins().brif(value, yes, &[], no, &[]);
        (yes, no)
    }
}

impl hirsel_core_layout::CaseOps for Code<'_, '_> {
    type V = Value;
    fn constant(&mut self, n: u64) -> Value {
        self.constant(n)
    }
    fn add(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().iadd(a, b)
    }
    fn sub(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().isub(a, b)
    }
    fn and(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().band(a, b)
    }
    fn below(&mut self, a: Value, b: Value) -> Value {
        let bit = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
        self.b.ins().uextend(I64, bit)
    }
    fn select(&mut self, c: Value, yes: Value, no: Value) -> Value {
        self.b.ins().select(c, yes, no)
    }
}
