//! `target.unsupported(why)` (PROG-6): an ordinary call of the bodiless declaration, which
//! no engine runs, since a build whose retained code reaches it is refused. The reason is
//! a string literal, put just before the call as a static at the call's anchor, so the
//! walk reads it from the retained body (`hirsel_core_ir::unsupported_reason`).
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn target_unsupported(&mut self, n: NodeId) -> Option<Option<ValueId>> {
        let argument = self.kids(n).get(1).copied().unwrap_or(n);
        let literal = self.kind(argument) == NodeKind::Literal
            && self.token_kind(argument) == Some(TokenKind::String);
        let Some(why) = literal
            .then(|| values::quoted_bytes(self.token_text(argument), false))
            .flatten()
        else {
            self.refuse(
                "PROG-6",
                argument,
                "target.unsupported takes its reason as a string literal",
            );
            return Some(None);
        };
        if self.live() {
            let data = self.e.static_bytes(&why);
            let marker = self.value(Ty::Ptr);
            self.inst(
                Inst::ReadStatic {
                    result: marker,
                    data,
                },
                n,
            );
        }
        None
    }
}
