//! Closed FORM runtime rows use their published C shapes. Layout descriptors
//! carry producer-computed storage, charge and ordinary fallible drop glue.
use super::*;
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl FunctionEmitter<'_> {
    fn value_descriptors(&mut self, types: &[TypeId]) -> String {
        let buffer = self.temp(&format!("[{} x i8]", 32 * types.len()));
        for (i, &t) in types.iter().enumerate() {
            let (bytes, align) = self.place_layout(PlaceTy::Declared(t));
            let logical = hirsel_core_layout::field_size(
                &self.module.types,
                self.recipes,
                &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
            )
            .2;
            for (j, value) in [bytes, align, logical].into_iter().enumerate() {
                let at = self.calc(format!(
                    "getelementptr i8, ptr {buffer}, i64 {}",
                    i * 32 + j * 8
                ));
                self.line(format!("store i64 {value}, ptr {at}, align 8"));
            }
            let at = self.calc(format!(
                "getelementptr i8, ptr {buffer}, i64 {}",
                i * 32 + 24
            ));
            self.line(format!("store ptr @core_drop_{}, ptr {at}, align 8", t.0));
        }
        buffer
    }
    pub(super) fn native_call(
        &mut self,
        name: &str,
        results: &[Ty],
        args: &[(String, String)],
    ) -> Vec<String> {
        let abi = abi::Return::new(results);
        let mut params = Vec::new();
        if abi.indirect {
            params.push(format!("ptr sret({})", abi.storage));
        }
        params.extend(args.iter().map(|(t, _)| t.clone()));
        self.intrinsics.insert(format!(
            "declare {} @{name}({})",
            abi.return_type(),
            params.join(", ")
        ));
        self.invoke(
            &abi,
            &format!("@{name}"),
            args.iter().map(|(t, v)| format!("{t} {v}")).collect(),
        )
    }
    fn host_call(&mut self, op: HostOp, args: &[String]) -> Vec<String> {
        let operand = args.get(1).map(String::as_str).unwrap_or("null");
        let mode = args.get(2).map(String::as_str).unwrap_or("0");
        let mut values = self.native_call(
            "hirsel_core_host_op",
            &[U, K, Ty::Scalar(Scalar::U32), U, K],
            &[
                ("ptr".into(), "%context".into()),
                ("i8 zeroext".into(), op.code().to_string()),
                ("i64".into(), args[0].clone()),
                ("ptr".into(), operand.into()),
                ("i8 zeroext".into(), mode.into()),
                ("i64".into(), "0".into()),
            ],
        );
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
                Arg::Value(v) => self.get(v),
                Arg::Address(a) => self.address(a),
            })
            .collect::<Vec<_>>();
        let p = |v: &str| ("ptr".into(), v.to_string());
        let u = |v: &str| ("i64".into(), v.to_string());
        let values = match op {
            RtOp::DestId => self.native_call("hirsel_core_dest_id", &[U], &[]),
            RtOp::ArgsLen => {
                self.native_call("hirsel_core_args_len", &[U], &[p("%context"), u(&args[0])])
            }
            RtOp::ArgsGet => self.native_call(
                "hirsel_core_args_get",
                &[P],
                &[p("%context"), u(&args[0]), u(&args[1])],
            ),
            RtOp::Host(op) => self.host_call(op, &args),
            RtOp::HashBytes => {
                self.native_call("hirsel_core_hash_bytes", &[U], &[p(&args[0]), u(&args[1])])
            }
            RtOp::CompareBytes => {
                vec![self.compare_bytes(&[p(&args[0]), u(&args[1]), p(&args[2]), u(&args[3])])]
            }
            RtOp::Spawn { x, r } => {
                let descriptor = self.value_descriptors(&[x, r]);
                self.native_call(
                    "hirsel_core_spawn",
                    &[P, U],
                    &[
                        p("%context"),
                        u(&args[0]),
                        p(&args[1]),
                        p(&args[2]),
                        ("ptr byval([64 x i8]) align 8".into(), descriptor),
                    ],
                )
            }
            RtOp::Join { .. } => self.native_call(
                "hirsel_core_join",
                &[K, P, K],
                &[p("%context"), p(&args[0]), p(&args[1]), u("0")],
            ),
            RtOp::Cancel => {
                self.native_call("hirsel_core_cancel", &[], &[p("%context"), p(&args[0])])
            }
            RtOp::CheckCancel => {
                self.native_call("hirsel_core_check_cancel", &[K], &[p("%context"), u("0")])
            }
            RtOp::ChannelNew { item } => {
                let descriptor = self.value_descriptors(&[item]);
                self.native_call(
                    "hirsel_core_channel_new",
                    &[P, P, U, K],
                    &[
                        p("%context"),
                        u(&args[0]),
                        ("ptr byval([32 x i8]) align 8".into(), descriptor),
                        u("0"),
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
                &[p("%context"), p(&args[0]), p(&args[1]), u("0")],
            ),
            RtOp::TrySend { .. } => {
                self.native_call("hirsel_core_try_send", &[K], &[p(&args[0]), p(&args[1])])
            }
        };
        let mut values = values;
        if op.fails() {
            let status = values.pop().unwrap();
            let failed = self.calc(format!("icmp ne i8 {status}, 0"));
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
