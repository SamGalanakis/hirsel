//! PACK-16 `Held`, resource-origin, and loaded static-literal LLVM lowering.

use super::super::*;
use hirsel_core_layout::{FieldTy, Repr, Scalar};

const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);

fn p(v: &Value) -> (Parameter, Value) {
    ptr(v.clone())
}
fn u(v: &Value) -> (Parameter, Value) {
    word(v.clone())
}

impl FunctionEmitter<'_> {
    pub(in crate::core) fn lower_held_op(
        &mut self,
        op: RtOp,
        args: &[Value],
    ) -> Option<Vec<Value>> {
        let out = match op {
            RtOp::ResourceOrigin { .. } if !self.module.track_pack_owners => vec![],
            RtOp::ResourceOrigin { item } => self.native_call(
                "hirsel_core_resource_origin",
                &[],
                &[
                    p(&args[0]),
                    ptr(Value::global(format!("core_resource_{}", item.0))),
                ],
            ),
            RtOp::HeldNew { item } => {
                let descriptor = self.value_descriptors(&[item], false);
                if self.recipes[item.index()].drop.is_none() {
                    let clean = self.offset(&descriptor, 24);
                    self.store(&Type::Ptr, &Value::Null, &clean, 8);
                }
                self.native_call(
                    "hirsel_core_held_new",
                    &[P, U],
                    &[
                        ptr(CONTEXT),
                        p(&args[0]),
                        p(&args[1]),
                        u(&args[2]),
                        ptr(descriptor),
                    ],
                )
            }
            RtOp::HeldTake { .. } => self.native_call(
                "hirsel_core_held_take",
                &[B],
                &[p(&args[0]), p(&args[1]), p(&args[2])],
            ),
            RtOp::StaticString => {
                if self.module.pack_module {
                    self.native_call(
                        "hirsel_core_static_string",
                        &[P],
                        &[p(&args[0]), u(&args[1])],
                    )
                } else {
                    vec![args[0].clone()]
                }
            }
            RtOp::StaticFrozen => {
                if self.module.pack_module {
                    self.native_call(
                        "hirsel_core_static_frozen",
                        &[P, P],
                        &[p(&args[0]), u(&args[2])],
                    )
                } else {
                    vec![args[0].clone(), args[1].clone()]
                }
            }
            RtOp::ReleaseStaticBytes => {
                self.native_call("hirsel_core_release_static_bytes", &[], &[p(&args[0])])
            }
            _ => return None,
        };
        Some(out)
    }

    pub(in crate::core) fn carrier_needs_drop(
        &mut self,
        ty: TypeId,
        p: &Value,
        capacity: &Value,
        memory: &Memory,
    ) -> Value {
        let owned = self.cmp(Predicate::Ne, &Type::I64, capacity, &int(0));
        let byte_seq = matches!(
            self.module.types.get(ty.index()).map(|s| (&s.repr, &s.body)),
            Some((Repr::Own, hirsel_core_layout::Body::Record(fields)))
                if matches!(
                    &fields[..],
                    [f] if matches!(&f.ty, FieldTy::Seq(e) if **e == FieldTy::Scalar(Scalar::U8))
                )
        );
        if byte_seq {
            let data = self.load_with(&Type::Ptr, p, memory);
            let non_null = self.cmp(Predicate::Ne, &Type::Ptr, &data, &Value::Null);
            self.bin(Binary::Or, &Type::I1, &owned, &non_null)
        } else {
            owned
        }
    }
}
