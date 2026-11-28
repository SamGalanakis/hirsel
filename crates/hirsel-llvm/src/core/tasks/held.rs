//! PACK-16 `Held`, resource-origin, and loaded static-literal LLVM lowering.

use super::super::*;
use hirsel_core_layout::Scalar;

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
                        &[P, U],
                        &[p(&args[0]), u(&args[1])],
                    )
                } else {
                    vec![args[0].clone(), int(0)]
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
            RtOp::ByteCapacity { carrier }
                if !(self.module.track_pack_owners
                    && hirsel_core_ir::byte_carrier(&self.module.types, carrier)) =>
            {
                vec![args[0].clone()]
            }
            RtOp::ByteCapacity { .. } => {
                let tagged = self.cmp(
                    Predicate::Uge,
                    &Type::I64,
                    &args[0],
                    &int(hirsel_core_layout::STRING_LITERAL_TAG),
                );
                vec![self.select(&Type::I64, &tagged, &int(0), &args[0])]
            }
            RtOp::ShareStaticFrozen if !self.module.track_pack_owners => vec![self.cmp(
                Predicate::Eq,
                &Type::I64,
                &args[0],
                &int(hirsel_core_layout::FROZEN_STATIC_UNCHARGED),
            )],
            RtOp::ShareStaticFrozen => {
                self.native_call("hirsel_core_share_static_frozen", &[B], &[u(&args[0])])
            }
            RtOp::ReleaseStaticBytes if !self.module.track_pack_owners => vec![],
            RtOp::ReleaseStaticBytes => {
                self.native_call("hirsel_core_release_static_bytes", &[], &[u(&args[0])])
            }
            RtOp::DropStaticStrings if !self.module.pack_module => vec![],
            RtOp::DropStaticStrings => self.native_call(
                "hirsel_core_drop_static_strings",
                &[],
                &[p(&args[0]), u(&args[1])],
            ),
            _ => return None,
        };
        Some(out)
    }
}
