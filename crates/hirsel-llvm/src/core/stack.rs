//! FAIL-17: the stack check at the entry of an activation of user code.
use super::*;

impl FunctionEmitter<'_> {
    /// FAIL-17: refuse this activation when the stack pointer, read after
    /// the prologue has reserved the whole frame, is below the run's limit.
    pub(super) fn stack_check(&mut self, fail: Option<BlockId>) {
        if hirsel_core_ir::stack_check_elided(self.body, |t| self.place_layout(t).0) {
            return;
        }
        let at = self.offset(&CONTEXT, memory_layout::CONTEXT_STACK_LIMIT as u64);
        let limit = self.load(&Type::I64, &at, 8);
        let top = self.intrinsic("llvm.stacksave.p0", Type::Ptr, &[]);
        let sp = self.cast(Cast::PtrToInt, &Type::Ptr, &top, &Type::I64);
        let room = self.cmp(Predicate::Uge, &Type::I64, &sp, &limit);
        let n = self.counter();
        let ready = Label::new("stack_ready", n);
        let slow = Label::new("stack_out", n);
        self.likely(&room, ready, slow);
        self.block(slow);
        let site = self.site_word();
        let status = self.runtime_bool(
            "hirsel_core_stack_out",
            &[ptr(CONTEXT), word(site)],
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
        self.branch_failure(&failed, fail, None);
        self.br(ready);
        self.block(ready);
    }
}
