//! Host row lowering for directory rights, registry admission and stat results.
use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn runtime_read_only_dir(&mut self, dir: ValueId, n: NodeId) -> Option<ValueId> {
        let ty = Ty::Scalar(Scalar::U64);
        let rights = self.konst(ty, hirsel_core_ids::host_op::DIR_READ_ONLY, n)?;
        let output = self.value(ty);
        self.inst(
            Inst::Binary {
                result: output,
                op: BinaryOp::BitOr,
                left: dir,
                right: rights,
                fail: None,
            },
            n,
        );
        Some(output)
    }

    pub(super) fn runtime_compiler(
        &mut self,
        op: HostOp,
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let args: Vec<_> = vals
            .iter()
            .map(|value| value.map(Arg::Value))
            .collect::<Option<_>>()?;
        let [value, error, _, available] = self.runtime_row(RtOp::Host(op), &args, n)?;
        let fits = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.raise_unless(fits, value, available, n);
        Some(value)
    }

    pub(super) fn runtime_stat(
        &mut self,
        ty: TyId,
        at: AddressId,
        kind: ValueId,
        size: ValueId,
        modified_ns: ValueId,
        n: NodeId,
    ) {
        let Some((_, tag)) = self.net_field(ty, at, 0, n) else {
            return;
        };
        self.runtime_store(at, &[Step::Field(1)], Ty::Scalar(Scalar::U64), size, n);
        self.runtime_store(
            at,
            &[Step::Field(2)],
            Ty::Scalar(Scalar::I64),
            modified_ns,
            n,
        );
        let join = self.new_block(&[]);
        for case in 0..3 {
            let Some(matches) = self.runtime_compare(Ty::Scalar(Scalar::U8), kind, case as u64, n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let next = self.new_block(&[]);
            self.runtime_branch(matches, yes, next, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: tag, case }, n);
            self.jump(join, &[], n);
            self.start(next);
        }
        self.inst(
            Inst::SetCase {
                address: tag,
                case: 3,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
