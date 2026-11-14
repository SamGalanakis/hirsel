//! Closed FORM runtime rows use their published C shapes. Layout descriptors
//! carry producer-computed storage, charge and ordinary fallible drop glue.
use super::storage::zeroext;
use super::*;
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl FunctionEmitter<'_> {
    fn value_descriptors(&mut self, types: &[TypeId], task_input: bool) -> Value {
        let buffer = self.temp(Type::bytes(32 * types.len() as u64));
        for (i, &t) in types.iter().enumerate() {
            let (mut bytes, mut align) = self.place_layout(PlaceTy::Declared(t));
            if task_input && i == 0 {
                bytes += task_entry::input_offset(self.module, self.recipes, t);
                align = align.max(8);
            }
            let logical = hirsel_core_layout::field_size(
                &self.module.types,
                self.recipes,
                &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
            )
            .2;
            for (j, value) in [bytes, align, logical].into_iter().enumerate() {
                let at = self.offset(&buffer, (i * 32 + j * 8) as u64);
                self.store(&Type::I64, &int(value), &at, 8);
            }
            let at = self.offset(&buffer, (i * 32 + 24) as u64);
            let drop = if task_input && i == 0 {
                format!("core_task_input_drop_{}", t.0)
            } else {
                format!("core_drop_c_{}", t.0)
            };
            self.store(&Type::Ptr, &Value::global(drop), &at, 8);
        }
        buffer
    }
    pub(super) fn native_call(&mut self, name: &str, results: &[Ty], args: &[CArg]) -> Vec<Value> {
        let abi = abi::Return::on_target(self.abi.target, results);
        let args: Vec<CArg> = args
            .iter()
            .map(|(p, v)| (self.c_param(p.clone()), v.clone()))
            .collect();
        let mut params = Vec::new();
        if abi.indirect {
            params.push(Parameter::sret(abi.storage.clone()));
        }
        params.extend(args.iter().map(|(p, _)| p.clone()));
        self.items
            .declare(name, abi.signature(params), FnAttrs::default());
        self.invoke(&abi, &Value::global(name.to_owned()), args)
    }
    fn host_call(&mut self, op: HostOp, args: &[Value]) -> Vec<Value> {
        let operand = args.get(1).cloned().unwrap_or(Value::Null);
        let mode = args.get(2).cloned().unwrap_or(int(0));
        let at = self.site_word();
        let results = [U, K, Ty::Scalar(Scalar::U32), U, K];
        let mut values = match op {
            HostOp::Rename => self.native_call(
                "hirsel_core_host_rename",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    ptr(args[1].clone()),
                    ptr(args[2].clone()),
                    word(at),
                ],
            ),
            HostOp::SleepUntil => self.native_call(
                "hirsel_core_host_sleep_until",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    word(args[1].clone()),
                    word(at),
                ],
            ),
            _ => self.native_call(
                "hirsel_core_host_op",
                &results,
                &[
                    ptr(CONTEXT),
                    zeroext(Type::I8, int(op.code())),
                    word(args[0].clone()),
                    ptr(operand),
                    zeroext(Type::I8, mode),
                    word(at),
                ],
            ),
        };
        if !op.waits() {
            values.truncate(1);
        }
        values
    }
    pub(super) fn runtime_call(&mut self, op: RtOp, site: &CallSite) -> Result<()> {
        let args = site
            .args
            .get(&self.body.pools.args)
            .unwrap()
            .iter()
            .map(|arg| match *arg {
                Arg::Value(v) => self.numeric(v),
                Arg::Address(a) => self.address(a),
            })
            .collect::<Vec<_>>();
        let at = if matches!(
            op,
            RtOp::Join { .. }
                | RtOp::CheckCancel
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
        ) {
            self.site_word()
        } else {
            int(0)
        };
        let p = |v: &Value| ptr(v.clone());
        let u = |v: &Value| word(v.clone());
        let values = match op {
            RtOp::IsolateEnter => {
                self.native_call("hirsel_core_isolate_enter", &[B, U], &[ptr(CONTEXT)])
            }
            RtOp::IsolateLeave => self.native_call("hirsel_core_isolate_leave", &[], &[ptr(CONTEXT)]),
            RtOp::IsolateCatch => {
                self.native_call("hirsel_core_isolate_catch", &[P, K], &[ptr(CONTEXT)])
            }
            RtOp::DestId => self.native_call("hirsel_core_dest_id", &[U], &[]),
            RtOp::ArgsLen => {
                self.native_call("hirsel_core_args_len", &[U], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::ArgsGet => self.native_call(
                "hirsel_core_args_get",
                &[P],
                &[ptr(CONTEXT), u(&args[0]), u(&args[1])],
            ),
            RtOp::Host(op) => self.host_call(op, &args),
            RtOp::CauseNext => self.native_call("hirsel_core_cause_next", &[P], &[p(&args[0])]),
            RtOp::CauseValue => self.native_call("hirsel_core_cause_value", &[P], &[p(&args[0])]),
            RtOp::HashBytes => {
                vec![self.hash_bytes(&args[0], &args[1])]
            }
            RtOp::CompareBytes => {
                vec![self.compare_bytes(&args[..4])]
            }
            RtOp::Spawn { x, r } => {
                let (bytes, _) = self.place_layout(PlaceTy::Declared(x));
                let offset = task_entry::input_offset(self.module, self.recipes, x);
                let packed = self.temp(Type::bytes(bytes + offset));
                self.store(&Type::Ptr, &args[2], &packed, 8);
                if bytes != 0 {
                    let data = self.offset(&packed, offset);
                    self.runtime_void(
                        "llvm.memcpy.p0.p0.i64",
                        &[
                            ptr(data),
                            ptr(args[1].clone()),
                            word(int(bytes)),
                            arg(Type::I1, FALSE),
                        ],
                    );
                }
                let descriptor = self.value_descriptors(&[x, r], true);
                self.native_call(
                    "hirsel_core_spawn",
                    &[P, U],
                    &[
                        ptr(CONTEXT),
                        u(&args[0]),
                        p(&packed),
                        ptr(Value::global(format!("core_task_entry_{}_{}", x.0, r.0))),
                        (abi::composite_argument(self.abi.target, 64), descriptor),
                    ],
                )
            }
            RtOp::Join { .. } => self.native_call(
                "hirsel_core_join",
                &[K, P, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
            ),
            RtOp::Cancel => {
                self.native_call("hirsel_core_cancel", &[], &[ptr(CONTEXT), p(&args[0])])
            }
            RtOp::CheckCancel => {
                self.native_call("hirsel_core_check_cancel", &[K], &[ptr(CONTEXT), u(&at)])
            }
            RtOp::ChannelNew { item } => {
                let descriptor = self.value_descriptors(&[item], false);
                self.native_call(
                    "hirsel_core_channel_new",
                    &[P, P, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[0]),
                        (abi::composite_argument(self.abi.target, 32), descriptor),
                        u(&at),
                    ],
                )
            }
            RtOp::ChannelSender => {
                self.native_call("hirsel_core_channel_sender", &[P], &[p(&args[0])])
            }
            RtOp::Send { .. } | RtOp::Receive { .. } => self.native_call(
                if matches!(op, RtOp::Send { .. }) {
                    "hirsel_core_send"
                } else {
                    "hirsel_core_receive"
                },
                &[B, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
            ),
            RtOp::TrySend { .. } => {
                self.native_call("hirsel_core_try_send", &[K], &[p(&args[0]), p(&args[1])])
            }
        };
        let mut values = values;
        if op.fails() {
            let status = values.pop().unwrap();
            let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
            self.branch_failure(&failed, site.fail, None);
        }
        for (&result, value) in site
            .results
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(result, &value);
        }
        Ok(())
    }
}
