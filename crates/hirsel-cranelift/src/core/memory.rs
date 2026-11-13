//! Calls to the landed core memory C ABI. Native layout is not a logical charge.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{Value, condcodes::IntCC};

impl Function<'_, '_, '_> {
    pub fn c_call(
        &mut self,
        name: &'static str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<calls::NativeValues> {
        self.calls
            .c(self.object, &mut self.b, name, params, results, args)
    }
    pub fn memory(&mut self, inst: &Inst) -> Result<()> {
        let p = Ty::Ptr;
        let u = Ty::Scalar(Scalar::U64);
        let boolean = Ty::Scalar(Scalar::Bool);
        match *inst {
            Inst::Alloc {
                result,
                available,
                region_entry,
                kind,
                place,
                charge,
                size,
                align,
                fail,
            } => {
                let ctx = self.context;
                let charge = self.get(charge);
                let size = self.get(size);
                let alignment = align;
                let align = self.b.ins().iconst(types::I64, align as i64);
                let payer = match place {
                    AllocPlace::Region(v) => self.get(v),
                    _ => self.b.ins().iconst(types::I64, 0),
                };
                // Allocation = {pointer, Capacity{requested,available}}, a C
                // struct of three words. On refusal only the pointer is null;
                // there is no guessed function status or call to the old runtime.
                let values = if let AllocKind::Resource { grant } = kind {
                    let storage = self.b.ins().iadd_imm_s(
                        charge,
                        -(hirsel_core_layout::charge::CAUSE_CELL as i64) - grant as i64,
                    );
                    let grant = self.b.ins().iconst(types::I64, grant as i64);
                    let resource = self.c_call(
                        "hirsel_core_resource_alloc",
                        &[p, u, u, u, u],
                        &[p, u, u],
                        &[ctx, size, align, storage, grant],
                    )?;
                    [resource[0], resource[1], resource[2]]
                } else {
                    let values = match place {
                        AllocPlace::Region(_) => {
                            self.region_alloc(payer, size, alignment, charge)?
                        }
                        _ => self.own_alloc(kind, place, size, alignment, charge)?,
                    };
                    [values[0], self.b.ins().iconst(types::I64, 0), values[1]]
                };
                self.set(result, values[0]);
                self.set(available, values[2]);
                if let Some(token) = region_entry {
                    // hirsel-core-rt abi::REGION_ENTRY, frozen on both 64-bit targets.
                    let token_value = self.b.ins().load(
                        types::I64,
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        payer,
                        memory_layout::REGION_ENTRY as i32,
                    );
                    self.set(token, token_value);
                }
                let refused = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
                let check = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(refused, check, &[], done, &[]);
                self.b.switch_to_block(check);
                let grant = self.c_call(
                    "hirsel_core_allocation_status",
                    &[p, p],
                    &[boolean],
                    &[ctx, values[0]],
                )?[0];
                self.guard(grant, 6, fail)?;
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                if matches!(kind, AllocKind::Counted(_)) {
                    let init = self.b.create_block();
                    let ready = self.b.create_block();
                    self.b.ins().brif(refused, ready, &[], init, &[]);
                    self.b.switch_to_block(init);
                    let one = self.b.ins().iconst(types::I64, 1);
                    self.b.ins().store(
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        one,
                        values[0],
                        memory_layout::NODE_COUNT as i32,
                    );
                    self.b.ins().jump(ready, &[]);
                    self.b.switch_to_block(ready);
                }
            }
            Inst::Drop { address, ty, fail } => {
                if let Some(glue) = self.glues.drops[ty.index()] {
                    let at = self.addr(address);
                    let glue = self.calls.reference(self.object, &mut self.b, glue);
                    let call = self.b.ins().call(glue, &[self.context, at]);
                    let status = self.b.inst_results(call)[0];
                    self.call_failure(status, fail)?;
                }
            }
            Inst::Share { node, kind } => {
                let node = self.get(node);
                fast::share(
                    &mut self.b,
                    &mut self.calls,
                    self.object,
                    node,
                    kind == Counted::Arc,
                )?;
            }
            Inst::TryUnwrap { result, node, kind } => {
                let node = self.get(node);
                let count = if kind == Counted::Arc {
                    self.b.ins().atomic_load(
                        types::I64,
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        node,
                    )
                } else {
                    self.b.ins().load(
                        types::I64,
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        node,
                        memory_layout::NODE_COUNT as i32,
                    )
                };
                let value = self.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
                if kind == Counted::Arc {
                    self.c_call("hirsel_core_arc_unwrapped", &[boolean], &[], &[value])?;
                }
                self.set(result, value);
            }
            Inst::Release { node, kind, ty } => {
                let node = self.get(node);
                let charge = hirsel_core_layout::charge::counted(
                    self.layouts[ty.index()].logical_size.unwrap(),
                )
                .unwrap();
                fast::release(
                    &mut self.b,
                    &mut self.calls,
                    self.object,
                    [self.context, node],
                    (charge, self.glues.resumes[ty.index()].unwrap()),
                    kind == Counted::Arc,
                )?;
            }
            Inst::Case { result, address } => {
                let value = self.case(address, None)?.unwrap();
                self.set(result, value);
            }
            Inst::SetCase { address, case } => {
                self.case(address, Some(case))?;
            }
            Inst::Trim {
                block: _,
                region,
                entry,
                previous,
                charge,
            } => {
                let region = self.get(region);
                let entry = self.get(entry);
                let previous = self.get(previous);
                let charge = self.get(charge);
                self.c_call(
                    "hirsel_core_region_lower_charge",
                    &[p, u, u, u],
                    &[],
                    &[region, entry, previous, charge],
                )?;
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
                if self.empty_bound {
                    let payer = self.current_limit();
                    let (ok, available_value) = self.admission(payer, bytes)?;
                    let zero = self.b.ins().iconst(types::I64, 0);
                    let room = self.b.ins().select(ok, zero, available_value);
                    self.set(admitted, ok);
                    self.set(available, room);
                    return Ok(());
                }
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
                if self.empty_bound {
                    return Ok(());
                }
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
