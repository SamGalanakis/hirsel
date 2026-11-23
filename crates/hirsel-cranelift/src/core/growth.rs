//! One shared runtime call when growth can reuse a backing, the entry LLVM
//! uses for every growth it does not complete inline. Other buffers retain
//! the inline allocation/copy/install/free sequence.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{StackSlotData, StackSlotKind, Value, condcodes::IntCC};
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);
const K: Ty = Ty::Scalar(Scalar::U8);

impl Function<'_, '_, '_> {
    #[allow(clippy::too_many_arguments)]
    pub fn grow_buffer(
        &mut self,
        kind: u8,
        stride: u64,
        at: Value,
        old: Value,
        size: Value,
        align: u64,
        charge: Value,
        overflow: Option<Value>,
        capacity: Value,
        moved: Value,
        refund: Value,
    ) -> Result<[Value; 2]> {
        let classic = self.b.create_block();
        let refused = self.b.create_block();
        self.b.set_cold_block(refused);
        self.b.append_block_param(refused, types::I64);
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let zero = self.b.ins().iconst(types::I64, 0);
        let dispatch = stride != 0;
        if dispatch {
            let cap = self.load_word(at, 16);
            let native = self.b.create_block();
            let large = self.b.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThanOrEqual,
                cap,
                memory_layout::GROW_CALL_MIN_PAYLOAD_BYTES.div_ceil(stride) as i64,
            );
            let reuse = if stride == 1 && kind == memory_layout::NATIVE_PLAIN {
                let slab = self.b.ins().icmp_imm_s(
                    IntCC::UnsignedLessThanOrEqual,
                    size,
                    memory_layout::GROW_SLAB_MAX_PAYLOAD_BYTES as i64,
                );
                self.b.ins().bor(large, slab)
            } else {
                large
            };
            self.b.ins().brif(reuse, native, &[], classic, &[]);
            self.b.switch_to_block(native);
            let alignment = self.b.ins().iconst(types::I64, align as i64);
            let stride_word = self.b.ins().iconst(types::I64, stride as i64);
            let kind = self.b.ins().iconst(types::I8, i64::from(kind));
            let over = overflow.unwrap_or_else(|| self.b.ins().iconst(types::I8, 0));
            let slot = self.b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                24,
                3,
            ));
            let shadow = self.b.ins().stack_addr(types::I64, slot, 0);
            let length = self.load_word(at, 8);
            self.store_word(shadow, 0, old);
            self.store_word(shadow, 8, length);
            self.store_word(shadow, 16, cap);
            let values = self.c_call(
                "hirsel_core_grow_cold",
                &[P, P, U, U, U, U, U, U, U, B, K],
                &[P, U],
                &[
                    self.context,
                    shadow,
                    size,
                    alignment,
                    stride_word,
                    charge,
                    capacity,
                    moved,
                    refund,
                    over,
                    kind,
                ],
            )?;
            // #93 guarantees the returned data on success, the exact requested
            // capacity, unchanged length, and untouched words on refusal.
            let null = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
            let data = self.b.ins().select(null, old, values[0]);
            let returned_capacity = self.b.ins().select(null, cap, capacity);
            self.store_word(at, 0, data);
            self.store_word(at, 16, returned_capacity);
            self.b.ins().brif(
                null,
                refused,
                &[values[1].into()],
                done,
                &[values[0].into(), zero.into()],
            );
        } else {
            self.b.ins().jump(classic, &[]);
        }
        self.b.switch_to_block(classic);
        let values = self.own_alloc(kind, size, align, charge, overflow)?;
        let null = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
        let copied = self.b.create_block();
        self.b
            .ins()
            .brif(null, refused, &[values[1].into()], copied, &[]);
        self.b.switch_to_block(copied);
        self.c_call("memcpy", &[P, P, U], &[P], &[values[0], old, moved])?;
        self.store_word(at, 0, values[0]);
        self.store_word(at, 16, capacity);
        fast::free(
            &mut self.b,
            &mut self.calls,
            self.object,
            self.context,
            old,
            refund,
        )?;
        self.b.ins().jump(done, &[values[0].into(), zero.into()]);
        self.b.switch_to_block(refused);
        let room = self.b.block_params(refused)[0];
        self.b.ins().jump(done, &[zero.into(), room.into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
}
