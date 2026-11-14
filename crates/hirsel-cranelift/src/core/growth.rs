//! One shared runtime call above the native payload threshold. Small buffers
//! retain the inline classic allocation/copy/install/free sequence.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{Value, condcodes::IntCC};
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
        fail: Option<BlockId>,
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
            let large = self.b.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThanOrEqual,
                cap,
                memory_layout::GROW_REALLOC_MIN_PAYLOAD_BYTES.div_ceil(stride) as i64,
            );
            let native = self.b.create_block();
            self.b.ins().brif(large, native, &[], classic, &[]);
            self.b.switch_to_block(native);
            let alignment = self.b.ins().iconst(types::I64, align as i64);
            let kind = self.b.ins().iconst(types::I8, i64::from(kind));
            let over = overflow.unwrap_or_else(|| self.b.ins().iconst(types::I8, 0));
            let values = self.c_call(
                "hirsel_core_grow",
                &[P, P, U, U, U, U, U, U, B, K],
                &[P, U],
                &[
                    self.context,
                    at,
                    size,
                    alignment,
                    charge,
                    capacity,
                    moved,
                    refund,
                    over,
                    kind,
                ],
            )?;
            let null = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
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
        let values = self.own_alloc(kind, Some(old), size, align, charge, overflow, dispatch)?;
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
        let status = self.c_call(
            "hirsel_core_allocation_status",
            &[P, P],
            &[B],
            &[self.context, zero],
        )?[0];
        self.guard(status, 6, fail)?;
        self.b.ins().jump(done, &[zero.into(), room.into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
}
