use super::*;

/// The runtime's error tags for `STDIN_SECRET` and the `SecretReadError` case
/// each becomes: NotTerminal, Busy, Empty, TooLong, Eof, then Aborted (7) and
/// TimedOut (6) as `stdin.read` gives them, then Policy (9). Any other tag is
/// `Other(code)`, case 8.
const CASES: [(u64, u32); 8] = [
    (1, 0),
    (2, 1),
    (3, 2),
    (4, 3),
    (5, 4),
    (7, 5),
    (6, 6),
    (9, 7),
];
const POLICY: u32 = 7;
const OTHER: u32 = 8;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// `stdin.read_secret(i, uses, until)`. The outcome's value is the
    /// Secret's designation on success. A policy refusal is tag 9 with code 1
    /// (Invalid) or 2 (Capacity, value requested and written available).
    pub(super) fn runtime_stdin_secret(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let mut operands = vec![vals[0]?, vals[1]?];
        operands.extend(self.net_until(self.p.fns[f].params[2].ty, vals[2]?, n)?);
        let args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
        let [value, error, code, written] =
            self.runtime_outputs::<4>(RtOp::Host(HostOp::StdinSecret), &args, n)?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], Ty::Scalar(Scalar::U64), value, n);
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_secret_read_error(at, ty, [error, code, value, written], n);
                }
            },
        )
    }

    fn runtime_secret_read_error(
        &mut self,
        to: AddressId,
        ty: TyId,
        [error, code, value, written]: [ValueId; 4],
        n: NodeId,
    ) {
        let byte = Ty::Scalar(Scalar::U8);
        let join = self.new_block(&[]);
        for (tag, case) in CASES {
            let Some(on) = self.runtime_compare(byte, error, tag, n) else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            if case == POLICY
                && let Some((_, at)) = self.net_case_field(ty, to, case, 0, n)
            {
                self.runtime_secret_error(at, code, value, written, n);
            }
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: OTHER,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField {
                case: OTHER,
                field: 0,
            }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
