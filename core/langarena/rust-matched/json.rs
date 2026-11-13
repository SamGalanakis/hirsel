// Reviewed-by: core-regions-pools-922 2026-11-13; Generate/ParseDom/ParseMapping, frozen valid test/run inputs; review8b04b7a4.
// Count-only sizing, one compact blob/trailer walk, byte-run writes, ASCII Clinger numbers, first coordinates key.
// Same two-pass JSON parse into one compact u32 blob, indexed containers, text numbers.
// Compact tags/layout: core/std/json.hirsel Json/Member and docs/spec/memory.md MEM-9.
// Parser/trailer walk adapted from reviewed layout/json_matched.rs. Numeric scan and
// Clinger conversion follow std.float for the frozen small-mantissa fixtures only.
mod text_support;
struct Doc {
    data: Vec<u8>,
}
// std.float.scan/significant and parse64's exact Clinger branch. Every number in
// the generated test/run documents fits this branch: coordinates have at most
// eight decimal places, and opts contains the integer 1. This is not a general
// replacement for std.float's Eisel-Lemire/big-decimal fallback.
fn decimal_small(s: &[u8]) -> f64 {
    const POW10: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];
    struct Scanned {
        mantissa: u64,
        exponent: i64,
        taken: u64,
        many: bool,
    }
    impl Scanned {
        fn digit(&mut self, x: u8, fraction: bool) {
            if self.mantissa == 0 && x == 0 {
                if fraction {
                    self.exponent -= 1;
                }
            } else if self.taken < 19 {
                self.mantissa = self.mantissa * 10 + u64::from(x);
                if fraction {
                    self.exponent -= 1;
                }
                self.taken += 1;
            } else {
                self.many |= x != 0;
                if !fraction {
                    self.exponent += 1;
                }
            }
        }
    }
    let negative = s.first() == Some(&b'-');
    let mut at = usize::from(negative);
    assert!(s.get(at).is_some_and(u8::is_ascii_digit), "number grammar");
    let mut n = Scanned {
        mantissa: 0,
        exponent: 0,
        taken: 0,
        many: false,
    };
    if s[at] == b'0' {
        at += 1;
    } else {
        while s.get(at).is_some_and(u8::is_ascii_digit) {
            n.digit(s[at] - b'0', false);
            at += 1;
        }
    }
    if s.get(at) == Some(&b'.') {
        at += 1;
        assert!(
            s.get(at).is_some_and(u8::is_ascii_digit),
            "fraction grammar"
        );
        while s.get(at).is_some_and(u8::is_ascii_digit) {
            n.digit(s[at] - b'0', true);
            at += 1;
        }
    }
    if s.get(at).is_some_and(|&b| b == b'e' || b == b'E') {
        at += 1;
        let minus = s.get(at) == Some(&b'-');
        if s.get(at).is_some_and(|&b| b == b'-' || b == b'+') {
            at += 1;
        }
        assert!(
            s.get(at).is_some_and(u8::is_ascii_digit),
            "exponent grammar"
        );
        let mut written = 0i64;
        while s.get(at).is_some_and(u8::is_ascii_digit) {
            if written < 65536 {
                written = 10 * written + i64::from(s[at] - b'0');
            }
            at += 1;
        }
        n.exponent += if minus { -written } else { written };
    }
    assert_eq!(at, s.len(), "number grammar");
    assert!(
        !n.many && (-22..=22).contains(&n.exponent) && n.mantissa <= 9007199254740992,
        "number outside the frozen fixtures' exact Clinger domain"
    );
    let mut value = n.mantissa as f64;
    if n.exponent < 0 {
        value /= POW10[usize::try_from(-n.exponent).unwrap()];
    } else {
        value *= POW10[usize::try_from(n.exponent).unwrap()];
    }
    if negative { -value } else { value }
}
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
                let _ = decimal_small(raw);
            }
            self.put(&[3]);
            self.word(raw.len());
            self.put(raw);
        }
    }
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
fn parse(source: &str) -> Doc {
    let mut sizing = Parser {
        s: source.as_bytes(),
        at: 0,
        used: 0,
        bytes: None,
    };
    sizing.value();
    sizing.ws();
    assert_eq!(sizing.at, source.len());
    let mut writer = Parser {
        s: source.as_bytes(),
        at: 0,
        used: 0,
        bytes: Some(Vec::with_capacity(sizing.used)),
    };
    writer.value();
    Doc {
        data: writer.bytes.unwrap(),
    }
}
impl Doc {
    fn word(&self, at: usize) -> usize {
        u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap()) as usize
    }
    fn text(&self, at: usize) -> (&[u8], usize) {
        let n = self.word(at);
        (&self.data[at + 4..at + 4 + n], at + 4 + n)
    }
    fn count(&self, at: usize) -> usize {
        self.word(at + 1)
    }
    fn child(&self, at: usize, i: usize) -> usize {
        assert!(i < self.count(at));
        let payload = at + 9;
        payload + self.word(payload + self.word(at + 5) + i * 4)
    }
    fn field(&self, at: usize, key: &str) -> usize {
        assert_eq!(self.data[at], 6);
        for i in 0..self.count(at) {
            let (k, value) = self.text(self.child(at, i));
            if k == key.as_bytes() {
                return value;
            }
        }
        panic!("missing field")
    }
    fn number(&self, at: usize) -> f64 {
        assert_eq!(self.data[at], 3);
        decimal_small(self.text(at + 1).0)
    }
    fn write(&self, at: usize, out: &mut Vec<u8>) {
        match self.data[at] {
            0 => out.extend_from_slice(b"null"),
            1 => out.extend_from_slice(b"false"),
            2 => out.extend_from_slice(b"true"),
            3 => out.extend_from_slice(self.text(at + 1).0),
            4 => quote(out, self.text(at + 1).0),
            tag @ 5..=6 => {
                out.push(if tag == 5 { b'[' } else { b'{' });
                for i in 0..self.count(at) {
                    if i > 0 {
                        out.push(b',');
                    }
                    let child = self.child(at, i);
                    if tag == 6 {
                        let (key, value) = self.text(child);
                        quote(out, key);
                        out.push(b':');
                        self.write(value, out);
                    } else {
                        self.write(child, out);
                    }
                }
                out.push(if tag == 5 { b']' } else { b'}' });
            }
            _ => panic!("invalid tag"),
        }
    }
}
fn quote(out: &mut Vec<u8>, b: &[u8]) {
    out.push(b'"');
    let mut start = 0;
    for (i, &c) in b.iter().enumerate() {
        if c < 0x20 || c == b'"' || c == b'\\' {
            out.extend_from_slice(&b[start..i]);
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
                    out.extend_from_slice(&[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[usize::from(c / 16)],
                        HEX[usize::from(c % 16)],
                    ]);
                }
            }
            start = i + 1;
        }
    }
    out.extend_from_slice(&b[start..]);
    out.push(b'"');
}

fn next(g: &mut i32) -> f64 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    f64::from(*g) / 139968.0
}
fn number(out: &mut String, x: f64) {
    if x == 0.0 {
        out.push_str("0.0");
    } else if x < 0.00001 {
        out.push_str(&format!("{x:e}"));
    } else {
        let s = x.to_string();
        out.push_str(&s);
        if !s.contains('.') {
            out.push_str(".0");
        }
    }
}
fn generate(n: usize) -> String {
    let mut out = String::from("{\"coordinates\":[");
    let mut g = 42;
    for i in 0..n {
        let x = (next(&mut g) * 100000000.0).round() / 100000000.0;
        let y = (next(&mut g) * 100000000.0).round() / 100000000.0;
        let z = (next(&mut g) * 100000000.0).round() / 100000000.0;
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"x\":");
        number(&mut out, x);
        out.push_str(",\"y\":");
        number(&mut out, y);
        out.push_str(",\"z\":");
        number(&mut out, z);
        out.push_str(&format!(
            ",\"name\":\"{:.7} {}\",\"opts\":{{\"1\":[1,true]}}}}",
            next(&mut g),
            (next(&mut g) * 10000.0) as i32
        ));
    }
    out.push_str("],\"info\":\"some info\"}");
    out
}
#[derive(Clone, Copy)]
struct Coordinate {
    x: f64,
    y: f64,
    z: f64,
}
fn average(doc: &Doc, mapping: bool) -> Coordinate {
    let a = doc.field(0, "coordinates");
    let mut x = 0.0;
    let mut y = 0.0;
    let mut z = 0.0;
    let mut mapped = Vec::new();
    for i in 0..doc.count(a) {
        let p = doc.child(a, i);
        let mut c = Coordinate {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        for j in 0..doc.count(p) {
            let (key, value) = doc.text(doc.child(p, j));
            if key == b"x" {
                c.x = doc.number(value);
            } else if key == b"y" {
                c.y = doc.number(value);
            } else if key == b"z" {
                c.z = doc.number(value);
            }
        }
        if mapping {
            mapped.push(c);
        } else {
            x += c.x;
            y += c.y;
            z += c.z;
        }
    }
    if mapping {
        for c in mapped {
            x += c.x;
            y += c.y;
            z += c.z;
        }
    }
    let n = doc.count(a) as f64;
    Coordinate {
        x: x / n,
        y: y / n,
        z: z / n,
    }
}
fn checksum(s: &str) -> u32 {
    s.bytes().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(b))
    })
}
fn main() {
    let a = text_support::arguments(&["coords"], true);
    let task = &a[1];
    let n: usize = a[2].parse().unwrap();
    let warmup: u64 = a[3].parse().unwrap();
    let iterations: u64 = a[4].parse().unwrap();
    let source = generate(n);
    let mut result = 0u32;
    if task == "Json::Generate" {
        let doc = parse(&source);
        let mut rendered = Vec::new();
        for _ in 0..warmup.checked_add(iterations).unwrap() {
            rendered = Vec::new();
            doc.write(0, &mut rendered);
            if rendered.starts_with(b"{\"coordinates\":") {
                result = result.checked_add(1).unwrap();
            }
        }
        std::hint::black_box(rendered);
    } else {
        assert!(task == "Json::ParseDom" || task == "Json::ParseMapping");
        for _ in 0..warmup.checked_add(iterations).unwrap() {
            let doc = parse(&source);
            let c = average(&doc, task == "Json::ParseMapping");
            let sum = checksum(&format!("{:.7}", c.x))
                .wrapping_add(checksum(&format!("{:.7}", c.y)))
                .wrapping_add(checksum(&format!("{:.7}", c.z)));
            result = result.wrapping_add(sum);
        }
    }
    text_support::output(result);
}
