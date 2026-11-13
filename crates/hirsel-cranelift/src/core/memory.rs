//! Calls to the landed core memory C ABI. Native layout is not a logical charge.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{Value, condcodes::IntCC};

impl Function<'_, '_, '_> {
    pub fn c_call(
        &mut self,
        name: &str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<Vec<Value>> {
        let sig = abi::Signature::c(params, results);
        let storage = sig.indirect.then(|| sig.storage(&mut self.b));
        let mut actual = Vec::new();
        if let Some(storage) = storage {
            actual.push(storage);
        }
        actual.extend_from_slice(args);
        let func = self
            .object
            .declare_function(name, Linkage::Import, &sig.clif)
            .map_err(error)?;
        let reference = self.object.declare_func_in_func(func, self.b.func);
        let inst = self.b.ins().call(reference, &actual);
        let values = self.b.inst_results(inst).to_vec();
        Ok(sig.unpack(&mut self.b, &values, storage))
    }
    pub fn memory(&mut self, inst: &Inst) -> Result<()> {
        let p = Ty::Ptr;
        let u = Ty::Scalar(Scalar::U64);
        let boolean = Ty::Scalar(Scalar::Bool);
        match *inst {
            Inst::Alloc {
                result,
                available,
                kind: AllocKind::Own,
                place,
                charge,
                size,
                align,
                fail,
            } => {
                let ctx = self.context;
                let charge = self.get(charge);
                let size = self.b.ins().iconst(types::I64, size as i64);
                let align = self.b.ins().iconst(types::I64, align as i64);
                let overflow = self.b.ins().iconst(types::I8, 0);
                let (name, payer) = match place {
                    AllocPlace::Current => {
                        ("hirsel_core_alloc", self.b.ins().iconst(types::I64, 0))
                    }
                    AllocPlace::Like(v) => ("hirsel_core_alloc", self.get(v)),
                    AllocPlace::Region(v) => ("hirsel_core_region_alloc", self.get(v)),
                };
                // Allocation = {pointer, Capacity{requested,available}}, a C
                // struct of three words. On refusal only the pointer is null;
                // there is no guessed function status or call to the old runtime.
                let values = self.c_call(
                    name,
                    &[p, p, u, u, u, boolean],
                    &[p, u, u],
                    &[ctx, payer, size, align, charge, overflow],
                )?;
                self.set(result, values[0]);
                self.set(available, values[2]);
                let refused = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
                let check = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(refused, check, &[], done, &[]);
                self.b.switch_to_block(check);
                let grant = self.c_call("hirsel_core_allocation_status", &[p, p], &[boolean], &[ctx, values[0]])?[0];
                self.guard(grant, 6, fail)?;
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            Inst::Case { result, address } => {
                let value = self.case(address, None)?.unwrap();
                self.set(result, value);
            }
            Inst::SetCase { address, case } => {
                self.case(address, Some(case))?;
            }
            Inst::Free { block, charge } => {
                let block = self.get(block);
                let charge = self.get(charge);
                self.c_call("hirsel_core_free_value", &[p, u], &[], &[block, charge])?;
            }
            Inst::Copy { to, from, bytes } => {
                let to = self.addr(to);
                let from = self.addr(from);
                let bytes = self.get(bytes);
                self.c_call("memcpy", &[p, p, u], &[p], &[to, from, bytes])?;
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let ctx = self.context;
                let bytes = self.get(bytes);
                let values = self.c_call(
                    "hirsel_core_limit_enter",
                    &[p, u],
                    &[p, u, u],
                    &[ctx, bytes],
                )?;
                let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, values[0], 0);
                self.set(admitted, ok);
                self.set(available, values[2]);
            }
            Inst::LimitExit => {
                self.c_call("hirsel_core_limit_exit", &[p], &[], &[self.context])?;
            }
            Inst::RegionOpen { result } => {
                // Physical chunk size is allocator policy and does not change
                // logical admission or the stable addresses it publishes.
                let chunk = self.b.ins().iconst(types::I64, 65536);
                let values = self.c_call("hirsel_core_region_open", &[u], &[p], &[chunk])?;
                self.set(result, values[0]);
            }
            Inst::RegionClose { region } => {
                let region = self.get(region);
                self.c_call("hirsel_core_region_close", &[p], &[], &[region])?;
            }
            _ => {
                return Err(unsupported(format!(
                    "core failure/count/resource runtime: {inst:?}"
                )));
            }
        }
        Ok(())
    }
}
