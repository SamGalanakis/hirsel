//! PACK-14/15 typed export ABI and runtime row emission for LLVM.
use super::abi::Return;
use super::storage::zeroext;
use super::*;

const P: Ty = Ty::Ptr;
const K: Ty = Ty::Scalar(Scalar::U8);

impl Return {
    /// PACK-15 typed export ABI: `(ctx, result_place, lowered_args...) -> zeroext i8`,
    /// no scalar-record rewrites, no `OkResult` splitting, no `sret`, no trailing caller site.
    pub fn pack_export(target: hirsel_core_layout::Target) -> Self {
        Self::on_target(target, &[Ty::Scalar(Scalar::U8)])
    }

    pub fn pack_export_parameters(
        &self,
        sig: &Signature,
        facts: bool,
        extent: impl Fn(PlaceTy) -> u64,
    ) -> (Vec<Parameter>, Vec<Local>) {
        debug_assert!(
            sig.results.is_empty(),
            "PACK-15 export signatures carry non-ZST results through dest"
        );
        let mut params = vec![
            Parameter::task_context(),
            Parameter {
                noalias: facts && sig.dest.is_some(),
                ..Parameter::plain(Type::Ptr)
            },
        ];
        let mut locals = vec![Local::CONTEXT, Local::DEST];
        for (i, p) in sig.params.iter().enumerate() {
            params.push(match p {
                Param::Value(t) => self.value_parameter(*t),
                Param::Address { loan, place } if facts => Parameter {
                    // PACK-15 explicitly permits overlapping frozen `view` places at the
                    // same address; only exclusive loans (`mut` / `take`) carry `noalias`.
                    noalias: *loan != Loan::View,
                    dereferenceable: extent(*place),
                    readonly: *loan == Loan::View,
                    ..Parameter::plain(Type::Ptr)
                },
                Param::Address { .. } => Type::Ptr.into(),
            });
            locals.push(Local::Param(i));
        }
        (params, locals)
    }
}

impl FunctionEmitter<'_> {
    pub(super) fn pack_export_call(&mut self, v: ValueId, site: &CallSite) -> Result<()> {
        let sig = &self.module.signatures[site.signature.index()];
        let abi = Return::pack_export(self.abi.target);
        let actual = site.args.get(&self.body.pools.args).unwrap();
        let mut args = vec![ptr(CONTEXT)];
        let rest = if sig.dest.is_some() {
            let Arg::Address(dest) = actual[0] else {
                unreachable!()
            };
            args.push(ptr(self.address(dest)));
            &actual[1..]
        } else {
            args.push(ptr(Value::Null));
            actual
        };
        for &a in rest {
            match a {
                Arg::Address(a) => args.push(ptr(self.address(a))),
                Arg::Value(v) => {
                    let x = self.numeric(v);
                    args.push((abi.value_parameter(self.vty(v)), x));
                }
            }
        }
        let callee = self.numeric(v);
        let mut values = self.invoke(&abi, &callee, args, &self.site_attrs(site));
        let status = values.pop().unwrap();
        let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
        self.branch_failure(&failed, site.fail, None);
        Ok(())
    }

    pub(super) fn pack_code_op(&mut self, op: RtOp, args: &[Value]) -> Vec<Value> {
        let p = |v: &Value| ptr(v.clone());
        let u = |v: &Value| word(v.clone());
        match op {
            RtOp::CodeBind => self.native_call("hirsel_core_code_bind", &[P], &[p(&args[0])]),
            RtOp::CodeLoad => self.native_call(
                "hirsel_core_code_load",
                &[K],
                &[
                    u(&args[0]),
                    p(&args[1]),
                    p(&args[2]),
                    p(&args[3]),
                    zeroext(Type::I8, args[4].clone()),
                    p(&args[5]),
                    p(&args[6]),
                    p(&args[7]),
                ],
            ),
            RtOp::CodeShare => self.native_call("hirsel_core_code_share", &[P], &[p(&args[0])]),
            RtOp::CodeSlotNew => {
                self.native_call("hirsel_core_code_slot_new", &[P], &[u(&args[0])])
            }
            RtOp::CodeActivate => self.native_call(
                "hirsel_core_code_activate",
                &[K],
                &[p(&args[0]), u(&args[1]), p(&args[2]), p(&args[3])],
            ),
            RtOp::CodeDispatchBegin => self.native_call(
                "hirsel_core_code_dispatch_begin",
                &[P],
                &[p(&args[0]), p(&args[1])],
            ),
            RtOp::CodeDispatchEnd => {
                self.native_call("hirsel_core_code_dispatch_end", &[], &[u(&args[0])])
            }
            RtOp::CodeIdentity => self.native_call(
                "hirsel_core_code_identity",
                &[],
                &[p(&args[0]), p(&args[1])],
            ),
            RtOp::CodeRevision => self.native_call(
                "hirsel_core_code_revision",
                &[K],
                &[p(&args[0]), p(&args[1])],
            ),
            _ => unreachable!("non-code RtOp passed to pack_code_op"),
        }
    }
}
