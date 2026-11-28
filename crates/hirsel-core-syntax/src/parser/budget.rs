use super::*;

impl<const BORROWED: bool> Parser<'_, BORROWED> {
    fn step_argument_starts(&self) -> bool {
        matches!(
            self.token(1).kind,
            TokenKind::Ident
                | TokenKind::Integer
                | TokenKind::KwIf
                | TokenKind::KwMatch
                | TokenKind::KwTrue
                | TokenKind::KwFalse
        )
    }

    fn bytes_argument_starts(&self) -> bool {
        self.at(sym::BYTES) && self.word(1) != sym::LBRACKET
    }

    pub(super) fn call_argument(&mut self) -> Parsed<NodeId> {
        let st = self.pos;
        let mut token = None;
        let kind = if self.token(0).kind == TokenKind::Ident && self.word(1) == sym::COLON {
            token = Some(self.bump());
            self.expect(sym::COLON)?;
            if self.bytes_argument_starts() {
                return self.fail("LANG-5", "a bytes argument has no label");
            }
            NodeKind::LabelledArgument
        } else if self.bytes_argument_starts() {
            self.bump();
            NodeKind::BytesArgument
        } else if self.at(sym::STEPS) && self.step_argument_starts() {
            self.bump();
            NodeKind::StepsArgument
        } else {
            return self.expr(0);
        };
        let v = self.expr(0)?;
        self.node(st, kind, token, &[v])
    }

    pub(super) fn bytes_of(&mut self) -> Parsed<NodeId> {
        let start = self.pos;
        let keyword = self.bump();
        self.bump();
        self.whitespace += 1;
        let result = (|| {
            let ty = self.ty()?;
            self.expect(sym::RBRACKET)?;
            self.node(start, NodeKind::BytesOf, Some(keyword), &[ty])
        })();
        self.whitespace -= 1;
        result
    }

    /// The message for a missing name. `bytes[T]` written as a module member
    /// names its bare spelling.
    pub(super) fn name_message(&self) -> String {
        let found = self.text_token(self.index());
        if self.at(sym::BYTES) && self.word(1) == sym::LBRACKET {
            "`bytes[T]` is not a member of any module; write it bare, as `bytes[T]` (MEM-33)".into()
        } else if self.token(0).kind.is_keyword() {
            format!("`{found}` is a reserved word and cannot be used as a name")
        } else {
            format!("expected a name, found `{found}`")
        }
    }
}
