use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn runtime_net_error(
        &mut self,
        to: AddressId,
        ty: TyId,
        error: ValueId,
        code: ValueId,
        n: NodeId,
    ) {
        let join = self.new_block(&[]);
        for case in 0..7 {
            let Some(on) =
                self.runtime_compare(Ty::Scalar(Scalar::U8), error, u64::from(case + 1), n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        for (tag, case) in [(11, 10), (12, 11), (13, 12)] {
            let on = self
                .runtime_compare(Ty::Scalar(Scalar::U8), error, tag, n)
                .unwrap();
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            if tag == 12 {
                self.runtime_store(
                    to,
                    &[Step::CaseField { case, field: 0 }],
                    Ty::Scalar(Scalar::U32),
                    code,
                    n,
                );
            }
            self.jump(join, &[], n);
            self.start(no);
        }
        let tls = self
            .runtime_compare(Ty::Scalar(Scalar::U8), error, 9, n)
            .unwrap();
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.runtime_branch(tls, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 8,
            },
            n,
        );
        if let Some((ty, field)) = self.net_case_field(ty, to, 8, 0, n) {
            self.runtime_tls_error(ty, field, code, n);
        }
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 7,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 7, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
    fn runtime_tls_error(&mut self, ty: TyId, to: AddressId, code: ValueId, n: NodeId) {
        let k = Ty::Scalar(Scalar::U8);
        let word = Ty::Scalar(Scalar::U32);
        let case = self.value(k);
        self.inst(
            Inst::Convert {
                result: case,
                flag: None,
                operand: code,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let divisor = self.konst(word, 256, n).unwrap();
        let shifted = self.value(word);
        let flag = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: shifted,
                flag: Some(flag),
                op: ArithOp::Div,
                mode: ArithMode::Checked,
                left: code,
                right: divisor,
            },
            n,
        );
        let detail = self.value(k);
        self.inst(
            Inst::Convert {
                result: detail,
                flag: None,
                operand: shifted,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let join = self.new_block(&[]);
        for i in 0..6 {
            let on = self.runtime_compare(k, case, u64::from(i), n).unwrap();
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(
                Inst::SetCase {
                    address: to,
                    case: i,
                },
                n,
            );
            if i == 0 {
                let (_, certificate) = self.net_case_field(ty, to, 0, 0, n).unwrap();
                for case in 0..4 {
                    let on = self.runtime_compare(k, detail, u64::from(case), n).unwrap();
                    let yes = self.new_block(&[]);
                    let no = self.new_block(&[]);
                    self.runtime_branch(on, yes, no, n);
                    self.start(yes);
                    self.inst(
                        Inst::SetCase {
                            address: certificate,
                            case,
                        },
                        n,
                    );
                    self.jump(join, &[], n);
                    self.start(no);
                }
                self.inst(
                    Inst::SetCase {
                        address: certificate,
                        case: 4,
                    },
                    n,
                );
            } else if i == 4 {
                self.runtime_store(to, &[Step::CaseField { case: 4, field: 0 }], k, detail, n);
            }
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 6,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
    pub(super) fn runtime_stdin_error(
        &mut self,
        to: AddressId,
        error: ValueId,
        code: ValueId,
        n: NodeId,
    ) {
        let join = self.new_block(&[]);
        for (tag, case) in [(7, 0), (6, 1)] {
            let on = self
                .runtime_compare(Ty::Scalar(Scalar::U8), error, tag, n)
                .unwrap();
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 2,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 2, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
