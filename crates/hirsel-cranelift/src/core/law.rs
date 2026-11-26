use super::drop::Glue;
use super::function::Function;
use super::*;
use cranelift_codegen::ir::Value;
use cranelift_module::DataDescription;

impl Glue<'_, '_, '_> {
    pub(super) fn store_db_teardown(&mut self, index: usize, base: Value) -> Result<()> {
        if self.tables.input.store_teardown
            && self.tables.input.types[index].name == c::law::STORE_DB_TYPE
        {
            let status = self.call(
                "hirsel_core_test_db_teardown",
                &[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)],
                &[Ty::Scalar(Scalar::Bool)],
                &[self.ctx, base, self.caller_site],
            )?[0];
            self.merge(status);
        }
        Ok(())
    }
}

impl Function<'_, '_, '_> {
    pub(super) fn law_enter(&mut self, block: usize) -> Result<()> {
        if block == 0
            && self.input.store_teardown
            && let Some(hook) = c::law::store_teardown_hook(&self.instance.definition)
        {
            let params = self.body.blocks[0]
                .params
                .get(&self.body.pools.values)
                .unwrap();
            match hook {
                c::law::StoreTeardownHook::CheckDb => {
                    let db = self.get(params[0]);
                    let status = self.calls.c(
                        self.object,
                        &mut self.b,
                        "hirsel_core_test_db_teardown",
                        &[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)],
                        &[Ty::Scalar(Scalar::Bool)],
                        &[self.context, db, self.caller_site],
                    )?[0];
                    self.call_failure(status, None)?;
                }
                c::law::StoreTeardownHook::OptOut => {
                    let hi = self.get(params[0]);
                    let lo = self.get(params[1]);
                    self.calls.c(
                        self.object,
                        &mut self.b,
                        "hirsel_core_test_db_opt_out",
                        &[Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
                        &[],
                        &[hi, lo],
                    )?;
                }
                c::law::StoreTeardownHook::IsOptedOut => {}
            }
        }
        if self
            .law
            .as_ref()
            .is_some_and(|law| law.start.index() == block)
        {
            let law = self.law.take().unwrap();
            self.law_hook(law.callee, law.entry.operation, Some(&law.entry))?;
            self.law = Some(law);
        }
        Ok(())
    }

    pub(super) fn store_teardown_return(&mut self) -> Result<Option<Value>> {
        if self.input.store_teardown
            && c::law::store_teardown_hook(&self.instance.definition)
                == Some(c::law::StoreTeardownHook::IsOptedOut)
        {
            let params = self.body.blocks[0]
                .params
                .get(&self.body.pools.values)
                .unwrap();
            let hi = self.get(params[0]);
            let lo = self.get(params[1]);
            let value = self.calls.c(
                self.object,
                &mut self.b,
                "hirsel_core_test_db_is_opted_out",
                &[Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
                &[Ty::Scalar(Scalar::Bool)],
                &[hi, lo],
            )?[0];
            return Ok(Some(value));
        }
        Ok(None)
    }

    pub(super) fn law_leave(&mut self, returned: bool) -> Result<()> {
        if let Some(law) = self.law.take() {
            if law.callee.is_some() {
                if let Some(hook) = law.result.as_ref().filter(|_| returned) {
                    self.law_hook(law.callee, hook.operation, Some(hook))?;
                }
                self.law_hook(law.callee, hirsel_core_layout::law::LEAVE, None)?;
            }
            self.law = Some(law);
        }
        Ok(())
    }

    pub(super) fn law_hook(
        &mut self,
        callee: Option<u64>,
        operation: u64,
        hook: Option<&c::law::Hook>,
    ) -> Result<()> {
        use hirsel_core_layout::law;
        if let Some(hook) = hook
            && let Some(predicate) = hook.predicate
        {
            let label = self.get(hook.value);
            let predicate = self.get(predicate);
            self.calls.c(
                self.object,
                &mut self.b,
                "hirsel_core_law_witness",
                &[Ty::Ptr, Ty::Scalar(Scalar::Bool)],
                &[],
                &[label, predicate],
            )?;
            return Ok(());
        }
        let callee = self
            .b
            .ins()
            .iconst(types::I64, callee.unwrap_or(law::ABSENT) as i64);
        let (value, descriptor, len) = if let Some(hook) = hook {
            let mut data = DataDescription::new();
            data.define(
                hook.words
                    .iter()
                    .copied()
                    .flat_map(u64::to_le_bytes)
                    .collect(),
            );
            data.set_align(8);
            let id = self
                .object
                .declare_anonymous_data(false, false)
                .map_err(error)?;
            self.object.define_data(id, &data).map_err(error)?;
            let global = self.object.declare_data_in_func(id, self.b.func);
            let descriptor = self.b.ins().symbol_value(types::I64, global);
            (self.get(hook.value), descriptor, hook.words.len() as i64)
        } else {
            let null = self.b.ins().iconst(types::I64, 0);
            (null, null, 0)
        };
        let op = self.b.ins().iconst(types::I64, operation as i64);
        let len = self.b.ins().iconst(types::I64, len);
        let site = if operation == law::ENTER {
            self.caller_site
        } else {
            self.b.ins().iconst(types::I64, 0)
        };
        self.calls.c(
            self.object,
            &mut self.b,
            "hirsel_core_law_observe",
            &[
                Ty::Ptr,
                Ty::Scalar(Scalar::U64),
                Ty::Scalar(Scalar::U64),
                Ty::Scalar(Scalar::U64),
                Ty::Ptr,
                Ty::Ptr,
                Ty::Scalar(Scalar::U64),
            ],
            &[],
            &[self.context, op, callee, site, value, descriptor, len],
        )?;
        Ok(())
    }
}
