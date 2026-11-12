//! Scanner and literal/trivia algorithms extracted from hirsel-lexer; new tokens and rules.
use crate::*;
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LexOutput {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}
pub fn lex(text: &str) -> LexOutput {
    let mut scanner = Scanner::new(text.as_bytes());
    while scanner.pos < text.len() {
        scanner.next();
    }
    scanner.finish()
}
struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    output: LexOutput,
}
impl<'a> Scanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            output: LexOutput::default(),
        }
    }
    fn span(&self, start: usize, end: usize) -> Span {
        Span {
            start: start as u32,
            end: end as u32,
        }
    }
    fn error(&mut self, rule: &str, start: usize, end: usize, message: &str) {
        self.output
            .diagnostics
            .push(Diagnostic::error(rule, self.span(start, end), message));
    }

    fn emit(&mut self, start: usize, kind: TokenKind) {
        self.output.tokens.push(Token {
            kind,
            span: self.span(start, self.pos),
        });
    }
    fn finish(mut self) -> LexOutput {
        self.output.tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(self.pos, self.pos),
        });
        self.output
    }
    fn starts(&self, spelling: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(spelling)
    }
    fn next(&mut self) {
        let start = self.pos;
        let errors = self.output.diagnostics.len();
        let kind = match self.bytes[self.pos] {
            b' ' | b'\t' => {
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| matches!(b, b' ' | b'\t'))
                {
                    self.pos += 1;
                }
                TokenKind::Trivia(TriviaKind::Space)
            }
            b'\n' => {
                self.pos += 1;
                TokenKind::Trivia(TriviaKind::Newline)
            }
            b'\r' if self.starts(b"\r\n") => {
                self.pos += 2;
                TokenKind::Trivia(TriviaKind::Newline)
            }
            b'/' if self.starts(b"//") => self.line_comment(),
            b'/' if self.starts(b"/*") => self.block_comment(),
            b'r' if self.raw_start(false).is_some() => self.raw_string(false),
            b'b' if self.raw_start(true).is_some() => self.raw_string(true),
            b'"' => self.string(false),
            b'\'' if self.label_end().is_some() => self.label(),
            b'\'' => self.character_literal(false),
            b'b' if self.starts(b"b'") => self.character_literal(true),
            b'b' if self.starts(b"b\"") => self.string(true),
            b'0'..=b'9' => self.number(),
            b if letter(b) => {
                self.pos += 1;
                while self.bytes.get(self.pos).is_some_and(|b| name_byte(*b)) {
                    self.pos += 1;
                }
                // Identifier bytes are ASCII by construction.
                let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
                Keyword::from_spelling(text).map_or_else(|| TokenKind::Ident, TokenKind::Keyword)
            }
            _ => {
                if let Some((len, punct)) = punctuation(&self.bytes[self.pos..]) {
                    self.pos += len;
                    TokenKind::Punct(punct)
                } else {
                    let (_, len) = scalar(&self.bytes[self.pos..]);
                    self.pos += len;
                    self.error("LANG-1", start, self.pos, "invalid source character");
                    TokenKind::Invalid
                }
            }
        };
        let kind = if self.output.diagnostics.len() > errors {
            TokenKind::Invalid
        } else {
            kind
        };
        self.emit(start, kind);
    }
    fn label_end(&self) -> Option<usize> {
        let start = self.pos + 1;
        if !self.bytes.get(start).is_some_and(|&b| letter(b)) {
            return None;
        }
        let mut end = start + 1;
        while self.bytes.get(end).is_some_and(|&b| name_byte(b)) {
            end += 1;
        }
        // A closing apostrophe owns a character literal, including malformed ones.
        (self.bytes.get(end) != Some(&b'\'')).then_some(end)
    }
    fn label(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos = self.label_end().expect("checked label prefix");
        let name = std::str::from_utf8(&self.bytes[start + 1..self.pos]).unwrap();
        if name == "_" || Keyword::from_spelling(name).is_some() {
            self.error(
                "LANG-2",
                start,
                self.pos,
                "a label requires a non-keyword name other than underscore",
            );
        }
        TokenKind::Label
    }
    // Keep malformed spellings together through a quote or physical newline.
    // A byte literal (`byte`) is `b` and a character literal spelling one ASCII scalar.
    fn character_literal(&mut self, byte: bool) -> TokenKind {
        let start = self.pos;
        self.pos += 1 + usize::from(byte);
        while let Some(&byte) = self.bytes.get(self.pos) {
            if matches!(byte, b'\n' | b'\r') {
                break;
            }
            self.pos += 1;
            if byte == b'\'' {
                break;
            }
            if byte == b'\\'
                && self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| !matches!(b, b'\n' | b'\r'))
            {
                self.pos += 1;
            }
        }
        let spelling = std::str::from_utf8(&self.bytes[start..self.pos]).ok();
        let (valid, kind, message) = if byte {
            (
                spelling.and_then(crate::byte_value).is_some(),
                TokenKind::Byte,
                "byte literal must denote exactly one byte with a valid escape",
            )
        } else {
            (
                spelling.and_then(crate::character_value).is_some(),
                TokenKind::Character,
                "character literal must denote exactly one Unicode scalar with a valid escape",
            )
        };
        if !valid {
            self.error("LANG-1", start, self.pos, message);
        }
        kind
    }
    // Validate a source scalar while traversing a comment, including bare CR.
    fn comment_scalar(&mut self) {
        let start = self.pos;
        let (ch, len) = scalar(&self.bytes[start..]);
        self.pos += len;
        if ch.is_none()
            || ch
                .is_some_and(|c| bidi(c) || (c == '\r' && self.bytes.get(self.pos) != Some(&b'\n')))
        {
            self.error("LANG-1", start, self.pos, "forbidden character in comment");
        }
    }
    fn line_comment(&mut self) -> TokenKind {
        let doc = self.starts(b"///");
        self.pos += 2;
        while self.pos < self.bytes.len() && !matches!(self.bytes[self.pos], b'\r' | b'\n') {
            self.comment_scalar();
        }
        TokenKind::Trivia(if doc {
            TriviaKind::DocLine
        } else {
            TriviaKind::LineComment
        })
    }
    fn block_comment(&mut self) -> TokenKind {
        let start = self.pos;
        let doc = self.starts(b"/**");
        // /**/ overlaps the doc prefix's star with the closing delimiter (#21).
        self.pos += 2;
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            if self.starts(b"/*") {
                depth += 1;
                self.pos += 2;
            } else if self.starts(b"*/") {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    break;
                }
            } else {
                self.comment_scalar();
            }
        }
        if depth != 0 {
            self.error("LANG-1", start, self.pos, "unterminated block comment");
        }
        TokenKind::Trivia(if doc {
            TriviaKind::DocBlock
        } else {
            TriviaKind::BlockComment
        })
    }
    // Exponent and suffix validity is lexical (LANG-3).
    fn number(&mut self) -> TokenKind {
        let start = self.pos;
        let base = if self.starts(b"0x") {
            16
        } else if self.starts(b"0o") {
            8
        } else if self.starts(b"0b") {
            2
        } else {
            10
        };
        if base != 10 {
            self.pos += 2;
        }
        let mut valid = self.digits(base);
        let mut float = false;
        let projection = self
            .output
            .tokens
            .iter()
            .rev()
            .find(|t| !matches!(t.kind, TokenKind::Trivia(_)))
            .is_some_and(|t| t.kind == TokenKind::Punct(Punct::Dot));
        if base == 10 && !projection {
            if self.bytes.get(self.pos) == Some(&b'.')
                && self.bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit)
            {
                float = true;
                self.pos += 1;
                valid &= self.digits(10);
            }
            if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
                float = true;
                self.pos += 1;
                if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                valid &= self.digits(10);
            }
        }
        let suffix_start = self.pos;
        while self.bytes.get(self.pos).is_some_and(|b| name_byte(*b)) {
            self.pos += 1;
        }
        let suffix = &self.bytes[suffix_start..self.pos];
        if base == 10 && suffix.starts_with(b"f") {
            float = true;
        }
        valid &= if float {
            matches!(suffix, b"" | b"f32" | b"f64")
        } else {
            matches!(
                suffix,
                b"" | b"u8" | b"u16" | b"u32" | b"u64" | b"i8" | b"i16" | b"i32" | b"i64"
            )
        };
        if !valid {
            self.error(
                if float { "LANG-3" } else { "LANG-3" },
                start,
                self.pos,
                "malformed numeric literal or suffix",
            );
        }
        if float {
            TokenKind::Float
        } else {
            TokenKind::Integer
        }
    }
    fn digits(&mut self, base: u32) -> bool {
        let start = self.pos;
        let mut previous_digit = false;
        let mut valid = true;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if digit_value(byte).is_some_and(|d| d < base) {
                previous_digit = true;
            } else if byte == b'_' {
                valid &= previous_digit;
                previous_digit = false;
            } else {
                break;
            }
            self.pos += 1;
        }
        valid && previous_digit && self.pos > start
    }
    // Delimiter-counted strings retain their complete source spelling on the tape.
    fn raw_start(&self, bytes: bool) -> Option<usize> {
        let prefix = if bytes {
            b"br".as_slice()
        } else {
            b"r".as_slice()
        };
        let rest = self.bytes[self.pos..].strip_prefix(prefix)?;
        let hashes = rest.iter().take_while(|&&b| b == b'#').count();
        (rest.get(hashes) == Some(&b'"')).then_some(hashes)
    }
    fn raw_string(&mut self, bytes: bool) -> TokenKind {
        let start = self.pos;
        let hashes = self.raw_start(bytes).expect("checked raw prefix");
        self.pos += if bytes { 3 } else { 2 };
        self.pos += hashes;
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'"'
                && self.bytes[self.pos + 1..].iter().take(hashes).count() == hashes
                && self.bytes[self.pos + 1..self.pos + 1 + hashes]
                    .iter()
                    .all(|&b| b == b'#')
            {
                self.pos += 1 + hashes;
                return if bytes {
                    TokenKind::Bytes
                } else {
                    TokenKind::String
                };
            }
            let at = self.pos;
            let (ch, len) = scalar(&self.bytes[at..]);
            self.pos += len;
            if ch == Some('\r') {
                self.error(
                    "LANG-1",
                    at,
                    self.pos,
                    "raw literal newlines must be LF; CR and CRLF are forbidden",
                );
            } else if ch.is_none() || ch.is_some_and(|c| bidi(c) || (bytes && !c.is_ascii())) {
                self.error(
                    if ch.is_some_and(bidi) {
                        "LANG-1"
                    } else {
                        "LANG-3"
                    },
                    at,
                    self.pos,
                    "forbidden raw literal character",
                );
            }
        }
        self.error(
            "LANG-3",
            start,
            self.pos,
            "unterminated raw string or bytes literal",
        );
        TokenKind::Invalid
    }
    fn string(&mut self, bytes: bool) -> TokenKind {
        let start = self.pos;
        self.pos += if bytes { 2 } else { 1 };
        let mut closed = false;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    closed = true;
                    break;
                }
                // Leave the newline to the tape and resume on the next line.
                b'\r' | b'\n' => break,
                _ => self.literal_unit(bytes),
            }
        }
        if !closed {
            self.error(
                "LANG-3",
                start,
                self.pos,
                "unterminated string or bytes literal",
            );
        }
        if bytes {
            TokenKind::Bytes
        } else {
            TokenKind::String
        }
    }
    /// One escape or scalar of a quoted literal.
    fn literal_unit(&mut self, bytes: bool) {
        if self.bytes[self.pos] == b'\\' {
            self.escape(bytes);
            // A forbidden continuation is one invalid literal. Recover
            // through its closing quote while retaining the physical
            // newline bytes in that token's lossless span.
            if self.bytes.get(self.pos) == Some(&b'\r') {
                self.pos += 1;
                if self.bytes.get(self.pos) == Some(&b'\n') {
                    self.pos += 1;
                }
            } else if self.bytes.get(self.pos) == Some(&b'\n') {
                self.pos += 1;
            }
            return;
        }
        let at = self.pos;
        let (ch, len) = scalar(&self.bytes[at..]);
        self.pos += len;
        if ch.is_none()
            || ch.is_some_and(|c| {
                c <= '\u{1f}' || c == '\u{7f}' || bidi(c) || (bytes && !c.is_ascii())
            })
        {
            self.error(
                if ch.is_some_and(bidi) {
                    "LANG-1"
                } else {
                    "LANG-3"
                },
                at,
                self.pos,
                "forbidden literal character",
            );
        }
    }
    fn escape(&mut self, bytes: bool) {
        let start = self.pos;
        self.pos += 1;
        let Some(&byte) = self.bytes.get(self.pos) else {
            self.error("LANG-3", start, self.pos, "incomplete escape");
            return;
        };
        // Do not swallow a physical newline into a malformed escape.
        if matches!(byte, b'\r' | b'\n') {
            self.error(
                "LANG-3",
                start,
                self.pos,
                "escape line continuation is forbidden",
            );
            return;
        }
        match byte {
            b'\\' | b'"' | b'\'' | b'n' | b'r' | b't' | b'0' => {
                self.pos += 1;
            }
            b'x' => {
                self.pos += 1;
                let mut value = 0;
                let mut count = 0;
                while count < 2 {
                    let Some(digit) = self.bytes.get(self.pos).and_then(|b| digit_value(*b)) else {
                        break;
                    };
                    value = value * 16 + digit;
                    count += 1;
                    self.pos += 1;
                }
                if count != 2 || (!bytes && value > 0x7f) {
                    self.error("LANG-3", start, self.pos, "invalid hexadecimal escape");
                }
            }
            b'u' => {
                self.pos += 1;
                let mut valid = !bytes;
                if self.bytes.get(self.pos) == Some(&b'{') {
                    self.pos += 1;
                    let mut count = 0usize;
                    let mut value = Some(0u32);
                    while let Some(digit) = self.bytes.get(self.pos).and_then(|b| digit_value(*b)) {
                        count += 1;
                        value = value.and_then(|v| v.checked_mul(16)?.checked_add(digit));
                        self.pos += 1;
                    }
                    valid &= (1..=6).contains(&count) && value.and_then(char::from_u32).is_some();
                    if self.bytes.get(self.pos) == Some(&b'}') {
                        self.pos += 1;
                    } else {
                        valid = false;
                    }
                } else {
                    valid = false;
                }
                if !valid {
                    self.error("LANG-3", start, self.pos, "invalid Unicode escape");
                }
            }
            _ => {
                let (_, len) = scalar(&self.bytes[self.pos..]);
                self.pos += len;
                self.error("LANG-3", start, self.pos, "unknown escape");
            }
        }
    }
}
fn letter(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}
fn name_byte(byte: u8) -> bool {
    letter(byte) || byte.is_ascii_digit()
}
fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u32),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u32),
        _ => None,
    }
}
// Decode at most four bytes, so malformed byte streams still take linear time.
fn scalar(bytes: &[u8]) -> (Option<char>, usize) {
    let Some(&first) = bytes.first() else {
        return (None, 0);
    };
    let len = match first {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return (None, 1),
    };
    let ch = bytes
        .get(..len)
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.chars().next());
    if ch.is_some() { (ch, len) } else { (None, 1) }
}
fn bidi(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn punctuation(bytes: &[u8]) -> Option<(usize, Punct)> {
    // Longest match on at most three bytes: bounded work per source position (#21).
    let at = |i: usize| bytes.get(i).copied();
    let two = |second: u8, long: Punct, short: Punct| {
        if at(1) == Some(second) {
            (2, long)
        } else {
            (1, short)
        }
    };
    Some(match at(0)? {
        b'(' => (1, Punct::LParen),
        b')' => (1, Punct::RParen),
        b'[' => (1, Punct::LBracket),
        b']' => (1, Punct::RBracket),
        b'{' => (1, Punct::LBrace),
        b'}' => (1, Punct::RBrace),
        b',' => (1, Punct::Comma),
        b':' => (1, Punct::Colon),
        b';' => (1, Punct::Semicolon),
        b'@' => (1, Punct::At),
        b'?' => (1, Punct::Question),
        b'~' => (1, Punct::Tilde),
        b'.' => match (at(1), at(2)) {
            (Some(b'.'), Some(b'=')) => (3, Punct::DotDotEq),
            (Some(b'.'), _) => (2, Punct::DotDot),
            _ => (1, Punct::Dot),
        },
        b'<' => match (at(1), at(2)) {
            (Some(b'<'), Some(b'=')) => (3, Punct::ShlEq),
            (Some(b'<'), _) => (2, Punct::Shl),
            (Some(b'='), _) => (2, Punct::Le),
            _ => (1, Punct::Lt),
        },
        b'>' => match (at(1), at(2)) {
            (Some(b'>'), Some(b'=')) => (3, Punct::ShrEq),
            (Some(b'>'), _) => (2, Punct::Shr),
            (Some(b'='), _) => (2, Punct::Ge),
            _ => (1, Punct::Gt),
        },
        b'-' => match at(1) {
            Some(b'>') => (2, Punct::Arrow),
            Some(b'=') => (2, Punct::MinusEq),
            _ => (1, Punct::Minus),
        },
        b'=' => match at(1) {
            Some(b'>') => (2, Punct::FatArrow),
            Some(b'=') => (2, Punct::EqEq),
            _ => (1, Punct::Eq),
        },
        b'&' => match at(1) {
            Some(b'&') => (2, Punct::AndAnd),
            Some(b'=') => (2, Punct::AmpEq),
            _ => (1, Punct::Amp),
        },
        b'|' => match at(1) {
            Some(b'|') => (2, Punct::OrOr),
            Some(b'=') => (2, Punct::PipeEq),
            _ => (1, Punct::Pipe),
        },
        b'!' => two(b'=', Punct::Ne, Punct::Bang),
        b'+' => two(b'=', Punct::PlusEq, Punct::Plus),
        b'*' => two(b'=', Punct::StarEq, Punct::Star),
        b'/' => two(b'=', Punct::SlashEq, Punct::Slash),
        b'%' => two(b'=', Punct::PercentEq, Punct::Percent),
        b'^' => two(b'=', Punct::CaretEq, Punct::Caret),
        _ => return None,
    })
}
