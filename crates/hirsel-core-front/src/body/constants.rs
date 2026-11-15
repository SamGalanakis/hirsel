//! Published plain constants lowered as read-only native-layout data.
use super::*;

impl Cx<'_, '_, '_> {
    /// Serialize a published aggregate once per module, in its native recipe.
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
                    let Layout::Fixed(fixed) = &self.e.recipes[id.index()].layout else {
                        unreachable!()
                    };
                    let (size, align) = (fixed.size, fixed.align);
                    let mut bytes = vec![0; size as usize];
                    self.constant_bytes(value, &mut bytes, n)?;
                    let data = hirsel_core_ir::StaticId(self.e.module.statics.len() as u32);
                    self.e.module.statics.push(hirsel_core_ir::Static {
                        align: align as u32,
                        bytes,
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
    fn constant_bytes(
        &mut self,
        value: crate::ConstValue,
        bytes: &mut [u8],
        n: NodeId,
    ) -> Option<()> {
        match value.data {
            crate::ConstData::Bits(bits) => {
                let Rep::Scalar(ty) = self.rep_at(value.ty, n)? else {
                    self.gap(n, "a published field of bits without a scalar is not built");
                    return None;
                };
                let size = match ty {
                    Ty::Scalar(s) => s.size(),
                    Ty::Ptr => 8,
                } as usize;
                bytes[..size].copy_from_slice(&bits.to_le_bytes()[..size]);
            }
            crate::ConstData::Unit => {}
            crate::ConstData::Fields { start, len, case } => {
                let id = match self.rep_at(value.ty, n)? {
                    Rep::Place(id) => id,
                    // A zero-sized field takes no bytes.
                    Rep::None => return Some(()),
                    Rep::Scalar(_) => {
                        self.gap(n, "a published record held as a scalar is not built");
                        return None;
                    }
                };
                if let Some(case) = case {
                    let Layout::Fixed(fixed) = &self.e.recipes[id.index()].layout else {
                        unreachable!()
                    };
                    // A niche variant's dataful case is made by its fields alone.
                    let tag = fixed.tag.unwrap();
                    if let Some(word) = hirsel_core_layout::case_word(&tag, case) {
                        let offset = tag.offset as usize;
                        let width = tag.width as usize;
                        bytes[offset..offset + width].copy_from_slice(&word.to_le_bytes()[..width]);
                    }
                }
                let stride = if let TyKind::Array(elem, _) = *self.t.kind(value.ty) {
                    Some(match self.rep_at(elem, n)? {
                        Rep::None => 0,
                        Rep::Scalar(Ty::Scalar(s)) => s.size(),
                        Rep::Scalar(Ty::Ptr) => 8,
                        Rep::Place(id) => match &self.e.recipes[id.index()].layout {
                            Layout::Fixed(f) => f.size,
                            _ => unreachable!(),
                        },
                    })
                } else {
                    None
                };
                for i in 0..len as usize {
                    let field = self.p.constant_parts[start as usize + i];
                    let offset = if let Some(stride) = stride {
                        i as u64 * stride
                    } else {
                        let Layout::Fixed(fixed) = &self.e.recipes[id.index()].layout else {
                            unreachable!()
                        };
                        fixed.offsets[case.unwrap_or(0) as usize][i]
                    } as usize;
                    self.constant_bytes(field, &mut bytes[offset..], n)?;
                }
            }
        }
        Some(())
    }
}
