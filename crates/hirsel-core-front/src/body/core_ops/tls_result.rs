//! The Ok payloads of `tls.connect` and `tls.accept`: the two halves and
//! the chosen ALPN index.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn runtime_tls_result(
        &mut self,
        to: AddressId,
        ty: TyId,
        key: ValueId,
        protocol: ValueId,
        words: &[ValueId],
        n: NodeId,
    ) {
        let u = Ty::Scalar(Scalar::U64);
        let Some(pair) = self.runtime_halves(to, ty, key, n) else {
            return;
        };
        let Some((ty, handshake)) = self.net_field(ty, pair, 2, n) else {
            return;
        };
        let (_, selected) = self.net_field(ty, handshake, 0, n).unwrap();
        let (_, peer) = self.net_field(ty, handshake, 1, n).unwrap();
        for (i, word) in words.iter().enumerate() {
            self.runtime_store(peer, &[Step::Field(i as u32)], u, *word, n);
        }
        self.runtime_selected(selected, protocol, n);
    }

    /// `tls.accept`'s `(ReadHalf, WriteHalf, Option[u64])`: `protocol` is the
    /// chosen ALPN index plus one, or 0.
    pub(super) fn runtime_accept_result(
        &mut self,
        to: AddressId,
        ty: TyId,
        key: ValueId,
        protocol: ValueId,
        n: NodeId,
    ) {
        let Some(pair) = self.runtime_halves(to, ty, key, n) else {
            return;
        };
        let Some((_, selected)) = self.net_field(ty, pair, 2, n) else {
            return;
        };
        self.runtime_selected(selected, protocol, n);
    }

    /// Stores the read and write halves of `key` in the Ok payload's first two
    /// fields and returns the payload.
    fn runtime_halves(
        &mut self,
        to: AddressId,
        ty: TyId,
        key: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let u = Ty::Scalar(Scalar::U64);
        let pair = self.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)?;
        for direction in 1..=2 {
            let bit = self.konst(u, direction, n).unwrap();
            let half = self.value(u);
            self.inst(
                Inst::Binary {
                    result: half,
                    op: BinaryOp::BitOr,
                    left: key,
                    right: bit,
                    fail: None,
                },
                n,
            );
            self.runtime_store(pair, &[Step::Field(direction as u32 - 1)], u, half, n);
        }
        Some(pair)
    }

    /// An `Option[u64]` from an index plus one, or 0 for None.
    fn runtime_selected(&mut self, selected: AddressId, protocol: ValueId, n: NodeId) {
        let u = Ty::Scalar(Scalar::U64);
        let none = self.runtime_compare(u, protocol, 0, n).unwrap();
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: selected,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        let one = self.konst(u, 1, n).unwrap();
        let index = self.value(u);
        self.inst(
            Inst::IntArith {
                result: index,
                flag: None,
                op: ArithOp::Sub,
                mode: ArithMode::Wrapping,
                left: protocol,
                right: one,
            },
            n,
        );
        self.inst(
            Inst::SetCase {
                address: selected,
                case: 1,
            },
            n,
        );
        self.runtime_store(
            selected,
            &[Step::CaseField { case: 1, field: 0 }],
            u,
            index,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
