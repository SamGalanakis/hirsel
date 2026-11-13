// Hand-written safe Rust twin of std.json's two-pass, top-down compact encoding.
// Reviewed-by: core-collections-918, 2026-11-13, against layout/json.hirsel and std.json.
// Sizing-only validation and byte-oriented printer fixes rechecked; both valid-input benchmark phases match.
use std::str;
struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    used: usize,
    bytes: Option<Vec<u8>>,
}
impl Parser<'_> {
    fn put(&mut self, x: &[u8]) {
        self.used += x.len();
        if let Some(v) = &mut self.bytes {
            v.extend_from_slice(x);
        }
    }
    fn word(&mut self, n: usize) {
        self.put(&u32::try_from(n).unwrap().to_le_bytes());
    }
    fn patch(&mut self, at: usize, n: usize) {
        if let Some(v) = &mut self.bytes {
            v[at..at + 4].copy_from_slice(&u32::try_from(n).unwrap().to_le_bytes());
        }
    }
    fn peek(&self) -> u8 {
        self.s.get(self.at).copied().unwrap_or(0)
    }
    fn ws(&mut self) {
        while matches!(self.peek(), b' ' | b'\t' | b'\r' | b'\n') {
            self.at += 1;
        }
    }
    fn hex(&mut self) -> u32 {
        let mut cp = 0;
        for _ in 0..4 {
            let x = self.peek();
            self.at += 1;
            cp = cp * 16
                + match x {
                    b'0'..=b'9' => u32::from(x - b'0'),
                    b'a'..=b'f' => u32::from(x - b'a' + 10),
                    b'A'..=b'F' => u32::from(x - b'A' + 10),
                    _ => panic!("hex"),
                };
        }
        cp
    }
    fn text(&mut self) {
        if self.bytes.is_none() {
            assert_eq!(self.peek(), b'"');
        }
        self.at += 1;
        let header = self.used;
        self.word(0);
        let start = self.used;
        loop {
            let c = self.peek();
            if self.bytes.is_none() {
                assert!(c >= 32, "unterminated or control");
            }
            if c == b'"' {
                self.at += 1;
                break;
            }
            if c != b'\\' {
                let from = self.at;
                while self.peek() != b'"' && self.peek() != b'\\' {
                    if self.bytes.is_none() {
                        assert!(self.peek() >= 32);
                    }
                    self.at += 1;
                }
                self.put(&self.s[from..self.at]);
                continue;
            }
            self.at += 1;
            let esc = self.peek();
            self.at += 1;
            let ch = match esc {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let hi = self.hex();
                    let cp = if (0xd800..=0xdbff).contains(&hi) {
                        if self.bytes.is_none() {
                            assert_eq!(&self.s[self.at..self.at + 2], b"\\u");
                        }
                        self.at += 2;
                        let lo = self.hex();
                        if self.bytes.is_none() {
                            assert!((0xdc00..=0xdfff).contains(&lo));
                        }
                        0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                    } else {
                        hi
                    };
                    char::from_u32(cp).expect("scalar")
                }
                _ => panic!("escape"),
            };
            let mut buf = [0u8; 4];
            self.put(ch.encode_utf8(&mut buf).as_bytes());
        }
        self.patch(header, self.used - start);
    }
    fn value(&mut self) {
        self.ws();
        let c = self.peek();
        if c == b'n' || c == b't' || c == b'f' {
            let (word, tag): (&[u8], u8) = match c {
                b'n' => (b"null", 0),
                b't' => (b"true", 2),
                _ => (b"false", 1),
            };
            if self.bytes.is_none() {
                assert_eq!(&self.s[self.at..self.at + word.len()], word);
            }
            self.at += word.len();
            self.put(&[tag]);
        } else if c == b'"' {
            self.put(&[4]);
            self.text();
        } else if c == b'[' || c == b'{' {
            let object = c == b'{';
            let end = if object { b'}' } else { b']' };
            self.at += 1;
            self.put(&[if object { 6 } else { 5 }]);
            let header = self.used;
            self.word(0);
            self.word(0);
            let data = self.used;
            let mut count = 0;
            self.ws();
            if self.peek() != end {
                loop {
                    if object {
                        self.ws();
                        self.text();
                        self.ws();
                        if self.bytes.is_none() {
                            assert_eq!(self.peek(), b':');
                        }
                        self.at += 1;
                    }
                    self.value();
                    count += 1;
                    self.ws();
                    if self.peek() == end {
                        break;
                    }
                    if self.bytes.is_none() {
                        assert_eq!(self.peek(), b',');
                    }
                    self.at += 1;
                }
            }
            self.at += 1;
            let len = self.used - data;
            self.patch(header, count);
            self.patch(header + 4, len);
            if let Some(v) = &mut self.bytes {
                let mut at = data;
                for _ in 0..count {
                    let offset = at - data;
                    let next = if object {
                        let key = read32(v, at);
                        skip(v, at + 4 + key)
                    } else {
                        skip(v, at)
                    };
                    v.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
                    at = next;
                }
                self.used += 4 * count;
            } else {
                self.used += 4 * count;
            }
        } else {
            let start = self.at;
            while matches!(self.peek(), b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.at += 1;
            }
            let raw = &self.s[start..self.at];
            // Only sizing validates grammar/range. Writing consumes the accepted input.
            if self.bytes.is_none() {
                assert!(valid_number(raw));
                assert!(
                    str::from_utf8(raw)
                        .unwrap()
                        .parse::<f64>()
                        .unwrap()
                        .is_finite()
                );
            }
            self.put(&[3]);
            self.word(raw.len());
            self.put(raw);
        }
    }
}
fn valid_number(s: &[u8]) -> bool {
    let mut i = usize::from(s.first() == Some(&b'-'));
    if s.get(i) == Some(&b'0') {
        i += 1;
    } else {
        let start = i;
        while s.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while s.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if s.get(i).is_some_and(|x| *x == b'e' || *x == b'E') {
        i += 1;
        if s.get(i).is_some_and(|x| *x == b'+' || *x == b'-') {
            i += 1;
        }
        let start = i;
        while s.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == s.len()
}
fn read32(v: &[u8], at: usize) -> usize {
    u32::from_le_bytes(v[at..at + 4].try_into().unwrap()) as usize
}
fn skip(v: &[u8], at: usize) -> usize {
    match v[at] {
        0..=2 => at + 1,
        3 | 4 => at + 5 + read32(v, at + 1),
        5 | 6 => at + 9 + read32(v, at + 5) + 4 * read32(v, at + 1),
        _ => unreachable!(),
    }
}
fn parse(input: &str) -> Vec<u8> {
    let mut size = Parser {
        s: input.as_bytes(),
        at: 0,
        used: 0,
        bytes: None,
    };
    size.value();
    size.ws();
    assert_eq!(size.at, input.len());
    let mut write = Parser {
        s: input.as_bytes(),
        at: 0,
        used: 0,
        bytes: Some(Vec::with_capacity(size.used)),
    };
    write.value();
    write.bytes.unwrap()
}
fn quote(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let mut start = 0;
    for (i, c) in s.iter().copied().enumerate() {
        if c >= 32 && c != b'"' && c != b'\\' {
            continue;
        }
        out.extend_from_slice(&s[start..i]);
        start = i + 1;
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            8 => out.extend_from_slice(b"\\b"),
            9 => out.extend_from_slice(b"\\t"),
            10 => out.extend_from_slice(b"\\n"),
            12 => out.extend_from_slice(b"\\f"),
            13 => out.extend_from_slice(b"\\r"),
            _ => {
                const HEX: &[u8] = b"0123456789abcdef";
                out.reserve(6);
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(c / 16) as usize]);
                out.push(HEX[(c % 16) as usize]);
            }
        }
    }
    out.extend_from_slice(&s[start..]);
    out.push(b'"');
}
fn print(v: &[u8], at: usize, out: &mut Vec<u8>) {
    match v[at] {
        0 => out.extend_from_slice(b"null"),
        1 => out.extend_from_slice(b"false"),
        2 => out.extend_from_slice(b"true"),
        3 => out.extend_from_slice(&v[at + 5..at + 5 + read32(v, at + 1)]),
        4 => quote(out, &v[at + 5..at + 5 + read32(v, at + 1)]),
        tag @ 5..=6 => {
            let object = tag == 6;
            out.push(if object { b'{' } else { b'[' });
            let mut cursor = at + 9;
            for i in 0..read32(v, at + 1) {
                if i > 0 {
                    out.push(b',');
                }
                if object {
                    let n = read32(v, cursor);
                    quote(out, &v[cursor + 4..cursor + 4 + n]);
                    out.push(b':');
                    cursor += 4 + n;
                }
                print(v, cursor, out);
                cursor = skip(v, cursor);
            }
            out.push(if object { b'}' } else { b']' });
        }
        _ => unreachable!(),
    }
}
fn main() {
    let phase = std::env::args().nth(1).expect("phase");
    let input = std::fs::read_to_string(std::env::args().nth(2).expect("input")).unwrap();
    let sum: u64 = if phase == "json_parse" {
        (0..32).map(|_| read32(&parse(&input), 1) as u64).sum()
    } else if phase == "json_print" {
        let v = parse(&input);
        (0..32)
            .map(|_| {
                let mut out = Vec::new();
                print(&v, 0, &mut out);
                out.into_iter().map(u64::from).sum::<u64>()
            })
            .sum()
    } else {
        panic!("unknown phase")
    };
    println!("{sum}");
}
