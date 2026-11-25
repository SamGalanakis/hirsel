use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn law_enter(&mut self, block: usize) {
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
