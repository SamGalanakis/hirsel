//! Published plain constants lowered in declaration order.
use super::*;

impl Cx<'_, '_, '_> {
    /// Store a published aggregate once per module, before native layout.
    /// Every use reads the same read-only block; binding a mutable local copies it.
    pub(super) fn constant_value(
        &mut self,
        value: crate::ConstValue,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        match value.data {
            crate::ConstData::Bits(bits) => {
                let ty = self.ir_ty(value.ty)?;
                self.konst(ty, bits, n)
            }
            crate::ConstData::Unit => None,
            crate::ConstData::StaticString(_) => unreachable!("strings use their literal carrier"),
            crate::ConstData::Fields { .. } => {
                let data = if let Some(&id) = self.e.constant_statics.get(&value) {
                    id
                } else {
                    let id = match self.rep_at(value.ty, n)? {
                        Rep::Place(id) => id,
                        // A zero-sized aggregate has no value to read.
                        Rep::None => return None,
                        Rep::Scalar(_) => {
                            self.gap(n, "a published aggregate held as a scalar is not built");
                            return None;
                        }
                    };
                    let data = hirsel_core_ir::StaticId(self.e.module.statics.len() as u32);
                    let value_data = self.static_value(value);
                    self.e
                        .module
                        .statics
                        .push(hirsel_core_ir::Static::Aggregate {
                            ty: id,
                            value: value_data,
                        });
                    self.e.constant_statics.insert(value, data);
                    data
                };
                let result = self.value(Ty::Ptr);
                self.inst(Inst::ReadStatic { result, data }, n);
                Some(result)
            }
        }
    }
    /// Phase 0: a computed constant's slot. The body names the initializer; the
    /// value's bytes enter at emission (COMPTIME-DIRECTION section 3.3 rule 1).
    pub(super) fn computed_value(
        &mut self,
        init: hirsel_core_ids::DefPath,
        ty: crate::TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let rep = self.rep_at(ty, n)?;
        if matches!(rep, Rep::None) {
            return None;
        }
        if !self.out.computed.contains(&init) {
            self.out.computed.push(init.clone());
        }
        let data = hirsel_core_ir::StaticId(self.e.module.statics.len() as u32);
        self.e.module.statics.push(hirsel_core_ir::Static::Computed { init });
        let pointer = self.value(Ty::Ptr);
        self.inst(Inst::ReadStatic { result: pointer, data }, n);
        match rep {
            Rep::Scalar(t) => {
                let address = self.address(
                    hirsel_core_ir::AddressBase::Value(pointer),
                    &[],
                    hirsel_core_ir::PlaceTy::Value(t),
                    n,
                );
                Some(self.read_bits(address, t, n))
            }
            _ => Some(pointer),
        }
    }
    fn static_value(&self, value: crate::ConstValue) -> hirsel_core_ir::StaticValue {
        use hirsel_core_ir::StaticValue;
        match value.data {
            crate::ConstData::Bits(bits) => StaticValue::Bits(bits),
            crate::ConstData::StaticString(_) => unreachable!("constant aggregates are plain"),
            crate::ConstData::Unit => StaticValue::Fields {
                case: None,
                fields: vec![],
            },
            crate::ConstData::Fields { start, len, case } => StaticValue::Fields {
                case,
                fields: (start as usize..start as usize + len as usize)
                    .map(|i| self.static_value(self.p.constant_parts[i]))
                    .collect(),
            },
        }
    }
}
