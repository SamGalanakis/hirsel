use super::*;

impl Function<'_, '_, '_> {
    pub(super) fn argon2id(
        &mut self,
        args: &[Value; 13],
        fail: Option<BlockId>,
        outputs: &mut [Value; 8],
    ) -> Result<usize> {
        let mut operands = [self.context; 15];
        operands[1..14].copy_from_slice(args);
        operands[14] = self.site()?;
        let row = self.c_call(
            "hirsel_core_argon2id",
            &[P, P, U, P, U, P, U, P, U, W, W, W, U, P, U],
            &[U, U, K, K],
            &operands,
        )?;
        self.call_failure(row[3], fail)?;
        outputs[..3].copy_from_slice(&[row[0], row[1], row[2]]);
        Ok(3)
    }
}
