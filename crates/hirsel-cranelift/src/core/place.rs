//! Place lowering reads the producer's layout recipe. It never derives a second
//! layout, infers ownership, or checks its producer's field/type decisions.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value};
use hirsel_core_layout::{FieldTy, Fixed, Layout, Recipe, Repr};

#[derive(Clone)]
enum At {
    Other,
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(FieldTy),
}

fn fixed(layouts: &[Recipe], id: TypeId) -> Result<&Fixed> {
    match &layouts[id.index()].layout {
        Layout::Fixed(f) => Ok(f),
        Layout::Compact(_) => Err(unsupported(
            "compact value projection requires its extent recipe",
        )),
    }
}
fn held(input: &c::Module, id: TypeId) -> At {
    if input.types[id.index()].repr == Repr::Resource {
        At::Pointer(id)
    } else {
        At::Declared(id)
    }
}
fn field_at(input: &c::Module, t: &FieldTy) -> At {
    match t {
        FieldTy::Inline(t) => held(input, TypeId(t.0)),
        FieldTy::Box(t) => At::Pointer(TypeId(t.0)),
        FieldTy::Rc(t) | FieldTy::Arc(t) => At::Counted(TypeId(t.0)),
        FieldTy::Array(e, _) => At::Array((**e).clone()),
        _ => At::Other,
    }
}
pub(super) fn storage(input: &c::Module, layouts: &[Recipe], p: PlaceTy) -> Result<(u64, u64)> {
    Ok(match p {
        PlaceTy::Value(t) => {
            let n = u64::from(ty(t).bytes());
            (n, n)
        }
        PlaceTy::Declared(t) if input.types[t.index()].repr == Repr::Resource => (8, 8),
        PlaceTy::Declared(t) => {
            let f = fixed(layouts, t)?;
            (f.size, f.align)
        }
    })
}
impl Function<'_, '_, '_> {
    fn base_at(&self, a: &AddressInfo) -> Result<At> {
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
    fn next_at(&self, at: At, step: Step, ty: PlaceTy) -> Result<At> {
        Ok(match step {
            Step::Byte(_) => return Err(unsupported("compact byte projection is the next slice")),
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
                let At::Array(e) = at else {
                    unreachable!("array place")
                };
                field_at(self.input, &e)
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
    fn address_at(&self, id: AddressId) -> Result<At> {
        let a = &self.body.addresses[id.index()];
        let mut at = self.base_at(a)?;
        for &step in a.steps.get(&self.body.pools.steps).unwrap() {
            at = self.next_at(at, step, a.ty)?;
        }
        Ok(at)
    }
    pub fn address(&mut self, id: AddressId) -> Result<Value> {
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
                Step::Byte(_) => {
                    return Err(unsupported("compact byte projection is the next slice"));
                }
                Step::Deref => {
                    let p = self
                        .b
                        .ins()
                        .load(types::I64, MemFlagsData::trusted(), ptr, 0);
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
                Step::Element(_) => {
                    return Err(unsupported("inline array bounds require the failure ABI"));
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
        let native = match tag.width {
            1 => types::I8,
            2 => types::I16,
            _ => types::I32,
        };
        let address = self.addr(address);
        if let Some(value) = value {
            let value = self.b.ins().iconst(native, i64::from(value));
            self.b
                .ins()
                .store(MemFlagsData::trusted(), value, address, tag.offset as i32);
            Ok(None)
        } else {
            let value =
                self.b
                    .ins()
                    .load(native, MemFlagsData::trusted(), address, tag.offset as i32);
            Ok(Some(self.resize(value, types::I32, false)))
        }
    }
}
