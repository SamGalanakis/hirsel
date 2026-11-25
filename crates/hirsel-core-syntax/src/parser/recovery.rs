use super::*;

fn closing_delimiter(word: Symbol) -> Option<Symbol> {
    match word {
        sym::LPAREN => Some(sym::RPAREN),
        sym::LBRACKET => Some(sym::RBRACKET),
        sym::LBRACE => Some(sym::RBRACE),
        _ => None,
    }
}

fn delimiter_slot(close: Symbol) -> usize {
    match close {
        sym::RPAREN => 0,
        sym::RBRACKET => 1,
        sym::RBRACE => 2,
        _ => unreachable!("delimiter indexes exist only for closing delimiters"),
    }
}

impl Parser<'_> {
    fn push_delimiter(&mut self, pos: usize, close: Symbol) {
        let index = self.delimiters.len();
        self.delimiters.push((pos, close));
        self.delimiter_openings[delimiter_slot(close)].push(index);
    }
    fn pop_delimiter(&mut self) {
        let Some((_, close)) = self.delimiters.pop() else {
            return;
        };
        let openings = &mut self.delimiter_openings[delimiter_slot(close)];
        debug_assert_eq!(openings.last(), Some(&self.delimiters.len()));
        openings.pop();
    }
    fn truncate_delimiters(&mut self, len: usize) {
        while self.delimiters.len() > len {
            self.pop_delimiter();
        }
    }
    pub(super) fn clear_delimiters(&mut self) {
        self.delimiters.clear();
        for openings in &mut self.delimiter_openings {
            openings.clear();
        }
    }
    // Only diagnostics and recovery need the delimiter stack. Replay each
    // consumed token once, retaining recovery's truncations between calls.
    fn sync_delimiters(&mut self) {
        for pos in self.delimiter_pos..self.pos {
            let word = self.tokens[self.significant[pos]].symbol;
            if let Some(close) = closing_delimiter(word) {
                self.push_delimiter(pos, close);
            } else if self
                .delimiters
                .last()
                .is_some_and(|&(_, close)| word == close)
            {
                self.pop_delimiter();
            }
        }
        self.delimiter_pos = self.pos;
    }
    pub(super) fn expected<T>(&mut self, word: Symbol) -> Parsed<T> {
        self.sync_delimiters();
        if let Some(&(opener, close)) = self.delimiters.last()
            && (word == close
                || word == sym::COMMA
                    && (self.eof()
                        || self.newline() && close != sym::RBRACE
                        || matches!(self.word(0), sym::RPAREN | sym::RBRACKET | sym::RBRACE)))
        {
            let required = self.symbols.text(close);
            let declared = if self.eof() {
                "end of file"
            } else {
                self.text_token(self.index())
            };
            let mut diagnostic = Diagnostic::error(
                "LANG-5",
                self.current.span,
                format!(
                    "expected `{required}` to close `{}`",
                    self.text_token(self.significant[opener])
                ),
            )
            .with_comparison(declared, required);
            diagnostic
                .causal
                .push(self.tokens[self.significant[opener]].span);
            self.diagnostics.push(diagnostic);
            return Err(());
        }
        self.fail_expected(
            "LANG-5",
            self.symbols.text(word),
            format!("expected `{}`", self.symbols.text(word)),
        )
    }
    pub(super) fn recover(&mut self, start: usize, item: bool) {
        self.sync_delimiters();
        let outer = self.delimiters.partition_point(|&(pos, _)| pos < start);
        let outer_braces = self.delimiter_openings[delimiter_slot(sym::RBRACE)]
            .partition_point(|&index| index < outer);
        if self.pos == start && !self.eof() && (item || !self.at(sym::RBRACE)) {
            self.bump();
        }
        while !self.eof() {
            self.sync_delimiters();
            if self.item_start() && self.newline() {
                break;
            }
            let inner_braces =
                self.delimiter_openings[delimiter_slot(sym::RBRACE)].len() - outer_braces;
            if inner_braces == 0 {
                if !item && (self.at(sym::RBRACE) || self.newline()) {
                    break;
                }
                if self.delimiters.len() == outer && self.at(sym::SEMICOLON) {
                    self.bump();
                    break;
                }
            }
            if matches!(self.word(0), sym::RPAREN | sym::RBRACKET | sym::RBRACE) {
                let openings = &self.delimiter_openings[delimiter_slot(self.word(0))];
                if let Some(&index) = openings.last()
                    && index >= outer
                {
                    self.truncate_delimiters(index + 1);
                }
            }
            self.bump();
        }
        self.sync_delimiters();
        self.truncate_delimiters(outer);
    }
}
