use super::function::Function;
use super::*;
use cranelift_module::DataDescription;

impl Function<'_, '_, '_> {
    pub(super) fn law_enter(&mut self, block: usize) -> Result<()> {
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
