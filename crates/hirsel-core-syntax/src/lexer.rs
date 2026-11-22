//! Scanner and literal/trivia algorithms extracted from hirsel-lexer; new tokens and rules.
use crate::*;
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LexOutput {
    pub tokens: Vec<Token>,
    pub significant: Vec<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// Length-framed semantic token bytes, shared by body and header reuse keys.
    pub canonical: Vec<u8>,
    /// Canonical byte offsets before each token and after the final token.
    pub canonical_offsets: Vec<u32>,
}
pub fn lex(text: &str, symbols: &mut Symbols) -> LexOutput {
    lex_derived(text, symbols, usize::MAX)
}
/// Lex `text` whose bytes from `derived` on are generated (SEM-10's codec items): there a
/// name may continue with `$`, which no source name has, so a generated name is no name a
/// program can write or collide with.
pub(crate) fn lex_derived(text: &str, symbols: &mut Symbols, derived: usize) -> LexOutput {
    symbols.reserve_for_source(text.len());
    let mut scanner = Scanner::new(text, symbols, derived);
    while scanner.pos < text.len() {
        scanner.next();
    }
    scanner.finish()
}
struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    output: LexOutput,
    symbols: &'a mut Symbols,
    newline: bool,
    derived: usize,
}
const BYTE_OTHER: u8 = 0;
const BYTE_IDENT: u8 = 1;
const BYTE_DIGIT: u8 = 2;
const BYTE_SPACE: u8 = 3;
const BYTE_NEWLINE: u8 = 4;
const BYTE_CLASS: [u8; 256] = byte_classes();

const fn byte_classes() -> [u8; 256] {
    let mut classes = [BYTE_OTHER; 256];
    let mut b = 0;
    while b < 256 {
        classes[b] = if b == b' ' as usize || b == b'\t' as usize {
            BYTE_SPACE
        } else if b == b'\n' as usize || b == b'\r' as usize {
            BYTE_NEWLINE
        } else if b >= b'0' as usize && b <= b'9' as usize {
            BYTE_DIGIT
        } else if (b >= b'a' as usize && b <= b'z' as usize)
            || (b >= b'A' as usize && b <= b'Z' as usize)
            || b == b'_' as usize
        {
            BYTE_IDENT
        } else {
            BYTE_OTHER
        };
        b += 1;
    }
    classes
}
impl<'a> Scanner<'a> {
    fn new(text: &'a str, symbols: &'a mut Symbols, derived: usize) -> Self {
        let bytes = text.as_bytes();
        Self {
            text,
            bytes,
            pos: 0,
            output: LexOutput {
                tokens: Vec::with_capacity(bytes.len() / 2 + 1),
                significant: Vec::with_capacity(bytes.len() / 2 + 1),
                diagnostics: vec![],
                canonical: Vec::with_capacity(bytes.len() + 8),
                canonical_offsets: vec![0],
            },
            symbols,
            newline: false,
            derived,
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

    fn emit(&mut self, start: usize, kind: TokenKind, symbol: Symbol) {
        if !kind.is_trivia() {
            self.output.significant.push(self.output.tokens.len());
        }
        self.output.tokens.push(Token {
            kind,
            span: self.span(start, self.pos),
            symbol,
            line_before: self.newline,
        });
        if !kind.is_trivia() && !matches!(kind, TokenKind::Eof) {
            let length = (self.pos - start + 1) as u64;
            self.output
                .canonical
                .extend_from_slice(&length.to_le_bytes());
            self.output.canonical.push(kind as u8);
            self.output
                .canonical
                .extend_from_slice(&self.bytes[start..self.pos]);
        }
        self.output
            .canonical_offsets
            .push(self.output.canonical.len() as u32);
        // No token spans a line feed (LANG-1), so only whitespace sets the flag.
        if !kind.is_trivia() {
            self.newline = false;
        }
    }
    fn finish(mut self) -> LexOutput {
        self.output.significant.push(self.output.tokens.len());
        self.output.tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(self.pos, self.pos),
            symbol: sym::NONE,
            line_before: self.newline,
        });
        self.output
            .canonical_offsets
            .push(self.output.canonical.len() as u32);
        self.output
    }
    fn starts(&self, spelling: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(spelling)
    }
    fn next(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            match byte {
                b' ' | b'\t' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    self.newline = true;
                }
                b'\r' if self.bytes.get(self.pos + 1) == Some(&b'\n') => {
                    self.pos += 2;
                    self.newline = true;
                }
                _ => break,
            }
        }
        if self.pos == self.bytes.len() {
            return;
        }
        let start = self.pos;
        let errors = self.output.diagnostics.len();
        let mut symbol = sym::NONE;
        let kind = match self.bytes[self.pos] {
            b'/' if self.starts(b"//") => self.line_comment(),
            b'/' if self.starts(b"/*") => self.block_comment_refusal(),
            b'r' if self.raw_start(false).is_some() => self.raw_string(false),
            b'b' if self.raw_start(true).is_some() => self.raw_string(true),
            b'"' => self.string(false),
            b'\'' if self.label_end().is_some() => {
                let kind = self.label();
                symbol = self.symbols.intern(&self.text[start + 1..self.pos]);
                kind
            }
            b'\'' => self.character_literal(false),
            b'b' if self.starts(b"b'") => self.character_literal(true),
            b'b' if self.starts(b"b\"") => self.string(true),
            b'0'..=b'9' => self.number(),
            b if letter(b) => {
                self.pos += 1;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|&b| name_byte(b) || (b == b'$' && self.pos >= self.derived))
                {
                    self.pos += 1;
                }
                symbol = self.symbols.intern(&self.text[start..self.pos]);
                TokenKind::identifier(symbol)
            }
            _ => {
                if let Some((len, punct)) = punctuation(&self.bytes[self.pos..]) {
                    self.pos += len;
                    symbol = punct.symbol();
                    punct
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
        self.emit(start, kind, symbol);
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
        let name = &self.text[start + 1..self.pos];
        if name == "_" || self.symbols.find(name).is_some_and(crate::keyword_symbol) {
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
            self.error(
                if spelling.is_some_and(|s| s.chars().any(bidi)) {
                    "LANG-1"
                } else {
                    "LANG-3"
                },
                start,
                self.pos,
                message,
            );
        }
        kind
    }
    // Validate a source scalar while traversing a comment to its line end.
    fn comment_scalar(&mut self) {
        let start = self.pos;
        if self.bytes[start].is_ascii() {
            self.pos += 1;
            return;
        }
        let (ch, len) = scalar(&self.bytes[start..]);
        self.pos += len;
        if ch.is_none_or(bidi) {
            self.error("LANG-1", start, self.pos, "forbidden character in comment");
        }
    }
    fn skip_to_line_end(&mut self) {
        while self.pos < self.bytes.len() && !matches!(self.bytes[self.pos], b'\r' | b'\n') {
            self.comment_scalar();
        }
    }
    fn line_comment(&mut self) -> TokenKind {
        let doc = self.starts(b"///");
        self.pos += 2;
        self.skip_to_line_end();
        if doc {
            TokenKind::DocLine
        } else {
            TokenKind::LineComment
        }
    }
    // Every line tokenizes on its own, so a block comment is one invalid token to its line end.
    fn block_comment_refusal(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos += 2;
        self.skip_to_line_end();
        self.error(
            "LANG-1",
            start,
            self.pos,
            "block comments are not Hirsel: every line tokenizes on its own, so use `//`",
        );
        TokenKind::Invalid
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
            .significant
            .last()
            .is_some_and(|&i| self.output.tokens[i].symbol == sym::DOT);
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
                "LANG-3",
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
            if matches!(self.bytes[at], b'\r' | b'\n') {
                self.error(
                    "LANG-1",
                    start,
                    at,
                    "a raw literal must close on its line: every line tokenizes on its own",
                );
                return TokenKind::Invalid;
            }
            if self.bytes[at].is_ascii() {
                self.pos += 1;
                continue;
            }
            let (ch, len) = scalar(&self.bytes[at..]);
            self.pos += len;
            if ch.is_none() || ch.is_some_and(|c| bidi(c) || (bytes && !c.is_ascii())) {
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
        if self.bytes[at].is_ascii() {
            let byte = self.bytes[at];
            self.pos += 1;
            if byte <= 0x1f || byte == 0x7f {
                self.error("LANG-3", at, self.pos, "forbidden literal character");
            }
            return;
        }
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
    BYTE_CLASS[byte as usize] == BYTE_IDENT
}
fn name_byte(byte: u8) -> bool {
    matches!(BYTE_CLASS[byte as usize], BYTE_IDENT | BYTE_DIGIT)
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

fn punctuation(bytes: &[u8]) -> Option<(usize, TokenKind)> {
    // Longest match on at most three bytes: bounded work per source position (#21).
    let at = |i: usize| bytes.get(i).copied();
    let two = |second: u8, long: TokenKind, short: TokenKind| {
        if at(1) == Some(second) {
            (2, long)
        } else {
            (1, short)
        }
    };
    Some(match at(0)? {
        b'(' => (1, TokenKind::LParen),
        b')' => (1, TokenKind::RParen),
        b'[' => (1, TokenKind::LBracket),
        b']' => (1, TokenKind::RBracket),
        b'{' => (1, TokenKind::LBrace),
        b'}' => (1, TokenKind::RBrace),
        b',' => (1, TokenKind::Comma),
        b':' => (1, TokenKind::Colon),
        b';' => (1, TokenKind::Semicolon),
        b'@' => (1, TokenKind::At),
        b'?' => match (at(1), at(2)) {
            (Some(b'='), Some(b'>')) => (3, TokenKind::QuestionFatArrow),
            _ => (1, TokenKind::Question),
        },
        b'~' => (1, TokenKind::Tilde),
        b'.' => match (at(1), at(2)) {
            (Some(b'.'), Some(b'=')) => (3, TokenKind::DotDotEq),
            (Some(b'.'), _) => (2, TokenKind::DotDot),
            _ => (1, TokenKind::Dot),
        },
        b'<' => match (at(1), at(2)) {
            (Some(b'<'), Some(b'=')) => (3, TokenKind::ShlEq),
            (Some(b'<'), _) => (2, TokenKind::Shl),
            (Some(b'='), _) => (2, TokenKind::Le),
            _ => (1, TokenKind::Lt),
        },
        b'>' => match (at(1), at(2)) {
            (Some(b'>'), Some(b'=')) => (3, TokenKind::ShrEq),
            (Some(b'>'), _) => (2, TokenKind::Shr),
            (Some(b'='), _) => (2, TokenKind::Ge),
            _ => (1, TokenKind::Gt),
        },
        b'-' => match at(1) {
            Some(b'>') => (2, TokenKind::Arrow),
            Some(b'=') => (2, TokenKind::MinusEq),
            _ => (1, TokenKind::Minus),
        },
        b'=' => match at(1) {
            Some(b'>') => (2, TokenKind::FatArrow),
            Some(b'=') => (2, TokenKind::EqEq),
            _ => (1, TokenKind::Eq),
        },
        b'&' => match at(1) {
            Some(b'&') => (2, TokenKind::AndAnd),
            Some(b'=') => (2, TokenKind::AmpEq),
            _ => (1, TokenKind::Amp),
        },
        b'|' => match at(1) {
            Some(b'|') => (2, TokenKind::OrOr),
            Some(b'=') => (2, TokenKind::PipeEq),
            _ => (1, TokenKind::Pipe),
        },
        b'!' => two(b'=', TokenKind::Ne, TokenKind::Bang),
        b'+' => two(b'=', TokenKind::PlusEq, TokenKind::Plus),
        b'*' => two(b'=', TokenKind::StarEq, TokenKind::Star),
        b'/' => two(b'=', TokenKind::SlashEq, TokenKind::Slash),
        b'%' => two(b'=', TokenKind::PercentEq, TokenKind::Percent),
        b'^' => two(b'=', TokenKind::CaretEq, TokenKind::Caret),
        _ => return None,
    })
}
