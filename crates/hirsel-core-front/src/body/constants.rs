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
    fn static_value(&self, value: crate::ConstValue) -> hirsel_core_ir::StaticValue {
        use hirsel_core_ir::StaticValue;
        match value.data {
            crate::ConstData::Bits(bits) => StaticValue::Bits(bits),
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
