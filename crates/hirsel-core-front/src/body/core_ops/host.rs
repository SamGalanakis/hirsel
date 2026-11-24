//! A host stat result written into its Result case.
use super::*;

impl Cx<'_, '_, '_> {
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
