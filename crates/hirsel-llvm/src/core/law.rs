use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn store_db_teardown(&mut self, t: usize, base: &Value, status: &Value) {
        if self.module.store_teardown
            && self.module.types[t].name == hirsel_core_ir::law::STORE_DB_TYPE
        {
            let failed = self.runtime(
                "hirsel_core_test_db_teardown",
                Type::I8,
                &[
                    ptr(CONTEXT),
                    ptr(base.clone()),
                    (Type::I64.into(), self.caller_site.clone()),
                ],
                &CallAttrs::default(),
            );
            let before = self.load(&Type::I8, status, 0);
            let after = self.bin(Binary::Or, &Type::I8, &before, &failed);
            self.store(&Type::I8, &after, status, 0);
        }
    }

    pub(super) fn law_enter(&mut self, block: usize) {
        if block == 0
            && self.module.store_teardown
            && let Some(func) = self.function
            && let Some(hook) = hirsel_core_ir::law::store_teardown_hook(
                &self.module.instances[self.module.functions[func.index()].instance.index()]
                    .definition,
            )
        {
            let params = self.body.blocks[0]
                .params
                .get(&self.body.pools.values)
                .unwrap();
            match hook {
                hirsel_core_ir::law::StoreTeardownHook::CheckDb => {
                    let db = self.numeric(params[0]);
                    let failed = self.runtime(
                        "hirsel_core_test_db_teardown",
                        Type::I8,
                        &[
                            ptr(CONTEXT),
                            ptr(db),
                            (Type::I64.into(), self.caller_site.clone()),
                        ],
                        &CallAttrs::default(),
                    );
                    let failed = self.cmp(Predicate::Ne, &Type::I8, &failed, &int(0));
                    self.branch_failure(&failed, None, None);
                }
                hirsel_core_ir::law::StoreTeardownHook::OptOut => {
                    let hi = self.numeric(params[0]);
                    let lo = self.numeric(params[1]);
                    self.runtime_void(
                        "hirsel_core_test_db_opt_out",
                        &[(Type::I64.into(), hi), (Type::I64.into(), lo)],
                    );
                }
                hirsel_core_ir::law::StoreTeardownHook::IsOptedOut => {}
            }
        }
        if self
            .law
            .as_ref()
            .is_some_and(|law| law.start.index() == block)
        {
            let law = self.law.take().unwrap();
            self.law_hook(law.callee, law.entry.operation, Some(&law.entry));
            self.law = Some(law);
        }
    }

    pub(super) fn store_teardown_return(&mut self) -> Option<Value> {
        if self.module.store_teardown
            && let Some(func) = self.function
            && hirsel_core_ir::law::store_teardown_hook(
                &self.module.instances[self.module.functions[func.index()].instance.index()]
                    .definition,
            ) == Some(hirsel_core_ir::law::StoreTeardownHook::IsOptedOut)
        {
            let params = self.body.blocks[0]
                .params
                .get(&self.body.pools.values)
                .unwrap();
            let hi = self.numeric(params[0]);
            let lo = self.numeric(params[1]);
            let value = self.runtime(
                "hirsel_core_test_db_is_opted_out",
                Type::I8,
                &[(Type::I64.into(), hi), (Type::I64.into(), lo)],
                &CallAttrs::default(),
            );
            return Some(value);
        }
        None
    }

    pub(super) fn law_leave(&mut self, returned: bool) {
        if let Some(law) = self.law.take() {
            if law.callee.is_some() {
                if let Some(hook) = law.result.as_ref().filter(|_| returned) {
                    self.law_hook(law.callee, hook.operation, Some(hook));
                }
                self.law_hook(law.callee, hirsel_core_layout::law::LEAVE, None);
            }
            self.law = Some(law);
        }
    }

    pub(super) fn law_hook(
        &mut self,
        callee: Option<u64>,
        operation: u64,
        hook: Option<&hirsel_core_ir::law::Hook>,
    ) {
        use hirsel_core_layout::law;
        if let Some(hook) = hook
            && let Some(predicate) = hook.predicate
        {
            let label = self.numeric(hook.value);
            let predicate = self.numeric(predicate);
            self.runtime_void(
                "hirsel_core_law_witness",
                &[ptr(label), arg(Type::I8, predicate)],
            );
            return;
        }
        let (value, descriptor, len) = if let Some(hook) = hook {
            let name = format!("hirsel.law.{}.{operation}", self.function.unwrap().0);
            self.items.global(Global {
                name: name.clone().into(),
                linkage: Linkage::Private,
                constant: true,
                externally_initialized: false,
                ty: Type::bytes(8 * hook.words.len() as u64),
                init: Some(Value::Bytes(
                    hook.words
                        .iter()
                        .copied()
                        .flat_map(u64::to_le_bytes)
                        .collect(),
                )),
                align: 8,
                absolute_symbol: false,
                section: None,
            });
            (
                self.numeric(hook.value),
                Value::global(name),
                hook.words.len() as u64,
            )
        } else {
            (Value::Null, Value::Null, 0)
        };
        let site = if operation == law::ENTER {
            self.caller_site.clone()
        } else {
            int(0)
        };
        let word = |value| (Type::I64.into(), value);
        self.runtime_void(
            "hirsel_core_law_observe",
            &[
                ptr(CONTEXT),
                word(int(operation)),
                word(int(callee.unwrap_or(law::ABSENT))),
                word(site),
                ptr(value),
                ptr(descriptor),
                word(int(len)),
            ],
        );
    }
}
