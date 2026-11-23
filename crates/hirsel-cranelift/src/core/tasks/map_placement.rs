use super::*;

impl Function<'_, '_, '_> {
    pub(super) fn map_fast_salt(&mut self, index: i32) -> Value {
        let at = memory_layout::CONTEXT_MAP_FAST_SALT as i32 + index * 8;
        self.b
            .ins()
            .load(types::I64, MemFlagsData::new(), self.context, at)
    }

    pub(super) fn map_fold(&mut self, a: Value, b: Value) -> Value {
        let lo = self.b.ins().imul(a, b);
        let hi = self.b.ins().umulhi(a, b);
        self.b.ins().bxor(lo, hi)
    }

    pub(super) fn map_place_bytes(&mut self, bytes: Value, len: Value) -> Value {
        let s0 = self.map_fast_salt(0);
        let s1 = self.map_fast_salt(1);
        let head = self.b.create_block();
        let chunk = self.b.create_block();
        let tail = self.b.create_block();
        for _ in 0..3 {
            self.b.append_block_param(head, types::I64);
        }
        self.b
            .ins()
            .jump(head, &[bytes.into(), len.into(), s0.into()]);
        self.b.switch_to_block(head);
        let p = self.b.block_params(head)[0];
        let n = self.b.block_params(head)[1];
        let z = self.b.block_params(head)[2];
        let more = self
            .b
            .ins()
            .icmp_imm_u(ir::condcodes::IntCC::UnsignedGreaterThan, n, 16);
        self.b.ins().brif(more, chunk, &[], tail, &[]);
        self.b.switch_to_block(chunk);
        let low = self.b.ins().load(types::I64, MemFlagsData::new(), p, 0);
        let high = self.b.ins().load(types::I64, MemFlagsData::new(), p, 8);
        let a = self.b.ins().bxor(z, low);
        let b = self.b.ins().bxor(s1, high);
        let next_z = self.map_fold(a, b);
        let next_p = self.b.ins().iadd_imm_s(p, 16);
        let next_n = self.b.ins().iadd_imm_s(n, -16);
        self.b
            .ins()
            .jump(head, &[next_p.into(), next_n.into(), next_z.into()]);
        self.b.switch_to_block(tail);

        let zero = self.b.ins().iconst(types::I64, 0);
        let eight = self.b.ins().iconst(types::I64, 8);
        let above_eight = self
            .b
            .ins()
            .icmp_imm_u(ir::condcodes::IntCC::UnsignedGreaterThan, n, 8);
        let low_len = self.b.ins().select(above_eight, eight, n);
        let high_len = self.b.ins().iadd_imm_s(n, -8);
        let high_len = self.b.ins().select(above_eight, high_len, zero);
        let w0 = self.map_place_tail(p, low_len);
        let high = self.b.ins().iadd_imm_s(p, 8);
        let w1 = self.map_place_tail(high, high_len);
        let a = self.b.ins().bxor(z, w0);
        let b = self.b.ins().bxor(s1, w1);
        let b = self.b.ins().iadd(b, len);
        let t = self.map_fold(a, b);
        let mix = self
            .b
            .ins()
            .iconst(types::I64, hirsel_core_layout::map_index::MIX as i64);
        self.map_fold(t, mix)
    }

    fn map_place_tail(&mut self, p: Value, n: Value) -> Value {
        let mut word = self.b.ins().iconst(types::I64, 0);
        let mut consumed = word;
        for (bytes, ty) in [
            (8, types::I64),
            (4, types::I32),
            (2, types::I16),
            (1, types::I8),
        ] {
            let next = self.b.ins().iadd_imm_s(consumed, bytes);
            let enough =
                self.b
                    .ins()
                    .icmp(ir::condcodes::IntCC::UnsignedGreaterThanOrEqual, n, next);
            let load = self.b.create_block();
            let done = self.b.create_block();
            self.b.append_block_param(done, types::I64);
            self.b.append_block_param(done, types::I64);
            self.b
                .ins()
                .brif(enough, load, &[], done, &[consumed.into(), word.into()]);
            self.b.switch_to_block(load);
            let at = self.b.ins().iadd(p, consumed);
            let element = self.b.ins().load(ty, MemFlagsData::new(), at, 0);
            let element = if bytes == 8 {
                element
            } else {
                self.b.ins().uextend(types::I64, element)
            };
            let shift = self.b.ins().ishl_imm(consumed, 3);
            let part = self.b.ins().ishl(element, shift);
            let combined = self.b.ins().bor(word, part);
            self.b.ins().jump(done, &[next.into(), combined.into()]);
            self.b.switch_to_block(done);
            consumed = self.b.block_params(done)[0];
            word = self.b.block_params(done)[1];
        }
        word
    }
}
