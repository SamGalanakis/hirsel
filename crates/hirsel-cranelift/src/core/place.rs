//! Place lowering reads the producer's layout recipe. It never derives a second
//! layout, infers ownership, or checks its producer's field/type decisions.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value};
use hirsel_core_layout::{FieldTy, Fixed, Layout, Recipe, Repr};

#[derive(Clone, Copy)]
enum At<'a> {
    Other,
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(&'a FieldTy, u64),
}

fn fixed(layouts: &[Recipe], id: TypeId) -> Result<&Fixed> {
    match &layouts[id.index()].layout {
        Layout::Fixed(f) => Ok(f),
        Layout::Compact(_) => Err(unsupported(
            "compact value projection requires its extent recipe",
        )),
    }
}
/// A resource or stable value is held as a pointer to its block (MEM-5, MEM-24).
fn by_block(input: &c::Module, id: TypeId) -> bool {
    matches!(input.types[id.index()].repr, Repr::Resource | Repr::Stable)
}
fn held<'a>(input: &c::Module, id: TypeId) -> At<'a> {
    if by_block(input, id) {
        At::Pointer(id)
    } else {
        At::Declared(id)
    }
}
fn field_at<'a>(input: &c::Module, t: &'a FieldTy) -> At<'a> {
    match t {
        FieldTy::Inline(t) => held(input, TypeId(t.0)),
        FieldTy::Box(t) => At::Pointer(TypeId(t.0)),
        FieldTy::Rc(t) | FieldTy::Arc(t) => At::Counted(TypeId(t.0)),
        FieldTy::Array(e, n) => At::Array(e, *n),
        _ => At::Other,
    }
}
pub(super) fn storage(input: &c::Module, layouts: &[Recipe], p: PlaceTy) -> Result<(u64, u64)> {
    Ok(match p {
        PlaceTy::Value(t) => {
            let n = u64::from(ty(t).bytes());
            (n, n)
        }
        PlaceTy::Declared(t) if by_block(input, t) => (8, 8),
        PlaceTy::Declared(t) => {
            let f = fixed(layouts, t)?;
            (f.size, f.align)
        }
    })
}
impl<'c> Function<'_, '_, 'c> {
    fn base_at(&self, a: &AddressInfo) -> Result<At<'c>> {
        Ok(match a.base {
            AddressBase::Slot(s) => match self.body.slots[s.index()] {
                PlaceTy::Declared(t) => held(self.input, t),
                _ => At::Other,
            },
            AddressBase::Address(a) => self.address_at(a)?,
            AddressBase::Value(_) if a.steps.len == 0 => match a.ty {
                PlaceTy::Declared(t) => At::Declared(t),
                _ => At::Other,
            },
            _ => At::Other,
        })
    }
    fn next_at(&self, at: At<'c>, step: Step, ty: PlaceTy) -> Result<At<'c>> {
        Ok(match step {
            Step::Byte(_) => match ty {
                PlaceTy::Declared(t) => At::Declared(t),
                _ => At::Other,
            },
            Step::Deref => match at {
                At::Pointer(t) => At::Declared(t),
                At::Counted(t) => held(self.input, t),
                _ => At::Other,
            },
            Step::Index(_) => match ty {
                PlaceTy::Declared(t) => held(self.input, t),
                _ => At::Other,
            },
            Step::Element(_) => {
                let At::Array(e, _) = at else {
                    unreachable!("array place")
                };
                field_at(self.input, e)
            }
            Step::Field(f) | Step::CaseField { field: f, .. } => {
                let At::Declared(t) = at else {
                    unreachable!("declared field place")
                };
                let case = match step {
                    Step::CaseField { case, .. } => Some(case),
                    _ => None,
                };
                field_at(
                    self.input,
                    &self.input.types[t.index()].fields(case).unwrap()[f as usize].ty,
                )
            }
        })
    }
    fn address_at(&self, id: AddressId) -> Result<At<'c>> {
        let a = &self.body.addresses[id.index()];
        let mut at = self.base_at(a)?;
        for &step in a.steps.get(&self.body.pools.steps).unwrap() {
            at = self.next_at(at, step, a.ty)?;
        }
        Ok(at)
    }
    pub fn address(&mut self, id: AddressId, fail: Option<BlockId>) -> Result<Value> {
        let a = &self.body.addresses[id.index()];
        let mut at = self.base_at(a)?;
        let mut ptr = match a.base {
            AddressBase::Slot(s) => self
                .b
                .ins()
                .stack_addr(types::I64, self.slots[s.index()], 0),
            AddressBase::Value(v) => self.get(v),
            AddressBase::Address(a) => self.addr(a),
        };
        for &step in a.steps.get(&self.body.pools.steps).unwrap() {
            ptr = match step {
                Step::Byte(v) => {
                    let offset = self.get(v);
                    let offset = self.resize(offset, types::I64, false);
                    self.b.ins().iadd(ptr, offset)
                }
                Step::Deref => {
                    let p = self.b.ins().load(types::I64, MemFlagsData::new(), ptr, 0);
                    if matches!(at, At::Counted(_)) {
                        self.b.ins().iadd_imm_s(p, 8)
                    } else {
                        p
                    }
                }
                Step::Field(f) | Step::CaseField { field: f, .. } => {
                    let At::Declared(t) = at else {
                        unreachable!("declared field place")
                    };
                    let case = match step {
                        Step::CaseField { case, .. } => case,
                        _ => 0,
                    };
                    let offset = fixed(self.layouts, t)?.offsets[case as usize][f as usize];
                    self.b.ins().iadd_imm_s(ptr, offset as i64)
                }
                Step::Index(v) => {
                    let stride = storage(self.input, self.layouts, a.ty)?.0;
                    let index = self.get(v);
                    let index = self.resize(index, types::I64, false);
                    let offset = self.b.ins().imul_imm_s(index, stride as i64);
                    self.b.ins().iadd(ptr, offset)
                }
                Step::Element(v) => {
                    let At::Array(e, n) = at else {
                        unreachable!("inline array")
                    };
                    let stride =
                        hirsel_core_layout::field_size(&self.input.types, self.layouts, e).0;
                    let i = self.get(v);
                    let i = self.resize(i, types::I64, false);
                    let outside = self.b.ins().icmp_imm_s(
                        ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                        i,
                        n as i64,
                    );
                    self.guard(outside, 1, fail)?;
                    let off = self.b.ins().imul_imm_s(i, stride as i64);
                    self.b.ins().iadd(ptr, off)
                }
            };
            at = self.next_at(at, step, a.ty)?;
        }
        Ok(ptr)
    }
    pub fn case(&mut self, address: AddressId, value: Option<u32>) -> Result<Option<Value>> {
        let At::Declared(t) = self.address_at(address)? else {
            unreachable!("variant place")
        };
        let tag = fixed(self.layouts, t)?.tag.unwrap();
        let address = self.addr(address);
        if let Some(value) = value {
            super::case::set_case(&mut self.b, &tag, value, address, MemFlagsData::new());
            Ok(None)
        } else {
            Ok(Some(super::case::case_value(
                &mut self.b,
                &tag,
                address,
                MemFlagsData::new(),
            )))
        }
    }
}
