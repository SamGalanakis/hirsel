// Matched twin of json.hirsel. Review pending langs-numeric-920 and922 layout review.
// Same two-pass JSON parse into one compact u32 blob, indexed containers, text numbers.
// Compact tags/layout: core/std/json.hirsel Json/Member and docs/spec/memory.md MEM-9.
mod text_support;
struct Doc {
    data: Vec<u8>,
}
struct Parser<'a> {
    source: &'a [u8],
    pos: usize,
    data: Option<Vec<u8>>,
    size: usize,
}
impl Parser<'_> {
    fn byte(&mut self, b: u8) {
        self.size = self.size.checked_add(1).unwrap();
        if let Some(d) = &mut self.data {
            d.push(b);
        }
    }
    fn word(&mut self, n: u32) {
        for b in n.to_le_bytes() {
            self.byte(b);
        }
    }
    fn patch(&mut self, at: usize, n: u32) {
        if let Some(d) = &mut self.data {
            d[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
    }
    fn whitespace(&mut self) {
        while self.pos < self.source.len() && b" \n\r\t".contains(&self.source[self.pos]) {
            self.pos += 1;
        }
    }
    fn scalar(&mut self, c: char) {
        let mut b = [0u8; 4];
        for &x in c.encode_utf8(&mut b).as_bytes() {
            self.byte(x);
        }
    }
    fn hex4(&mut self) -> u32 {
        let mut n = 0u32;
        for _ in 0..4 {
            let c = self.source[self.pos];
            self.pos += 1;
            n = n * 16 + char::from(c).to_digit(16).unwrap();
        }
        n
    }
    fn text(&mut self) {
        assert_eq!(self.source[self.pos], b'"');
        self.pos += 1;
        let start = self.size;
        self.word(0);
        loop {
            let b = self.source[self.pos];
            self.pos += 1;
            if b == b'"' {
                break;
            }
            if b == b'\\' {
                let e = self.source[self.pos];
                self.pos += 1;
                match e {
                    b'"' | b'\\' | b'/' => self.byte(e),
                    b'b' => self.byte(8),
                    b'f' => self.byte(12),
                    b'n' => self.byte(10),
                    b'r' => self.byte(13),
                    b't' => self.byte(9),
                    b'u' => {
                        let a = self.hex4();
                        let c = if (0xd800..=0xdbff).contains(&a) {
                            assert_eq!(&self.source[self.pos..self.pos + 2], b"\\u");
                            self.pos += 2;
                            let b = self.hex4();
                            assert!((0xdc00..=0xdfff).contains(&b));
                            0x10000 + (a - 0xd800) * 1024 + (b - 0xdc00)
                        } else {
                            a
                        };
                        self.scalar(char::from_u32(c).unwrap());
                    }
                    _ => panic!("invalid escape"),
                }
            } else {
                assert!(b >= 0x20);
                self.byte(b);
            }
        }
        self.patch(start, u32::try_from(self.size - start - 4).unwrap());
    }
    fn value(&mut self) {
        self.whitespace();
        match self.source[self.pos] {
            b'n' => {
                assert_eq!(&self.source[self.pos..self.pos + 4], b"null");
                self.pos += 4;
                self.byte(0);
            }
            b'f' => {
                assert_eq!(&self.source[self.pos..self.pos + 5], b"false");
                self.pos += 5;
                self.byte(1);
            }
            b't' => {
                assert_eq!(&self.source[self.pos..self.pos + 4], b"true");
                self.pos += 4;
                self.byte(2);
            }
            b'"' => {
                self.byte(4);
                self.text();
            }
            b'[' | b'{' => {
                let object = self.source[self.pos] == b'{';
                self.pos += 1;
                self.byte(if object { 6 } else { 5 });
                let count_at = self.size;
                self.word(0);
                let length_at = self.size;
                self.word(0);
                let payload = self.size;
                let mut offsets = Vec::new();
                self.whitespace();
                let close = if object { b'}' } else { b']' };
                if self.source[self.pos] != close {
                    loop {
                        offsets.push(u32::try_from(self.size - payload).unwrap());
                        if object {
                            self.whitespace();
                            self.text();
                            self.whitespace();
                            assert_eq!(self.source[self.pos], b':');
                            self.pos += 1;
                        }
                        self.value();
                        self.whitespace();
                        if self.source[self.pos] != b',' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                assert_eq!(self.source[self.pos], close);
                self.pos += 1;
                self.patch(count_at, u32::try_from(offsets.len()).unwrap());
                self.patch(length_at, u32::try_from(self.size - payload).unwrap());
                for offset in offsets {
                    self.word(offset);
                }
            }
            _ => {
                let start = self.pos;
                while self.pos < self.source.len()
                    && b"-+0123456789.eE".contains(&self.source[self.pos])
                {
                    self.pos += 1;
                }
                assert!(self.pos > start);
                self.byte(3);
                self.word(u32::try_from(self.pos - start).unwrap());
                for i in start..self.pos {
                    self.byte(self.source[i]);
                }
            }
        }
    }
}
fn parse(source: &str) -> Doc {
    let mut sizing = Parser {
        source: source.as_bytes(),
        pos: 0,
        data: None,
        size: 0,
    };
    sizing.value();
    sizing.whitespace();
    assert_eq!(sizing.pos, source.len());
    let mut writer = Parser {
        source: source.as_bytes(),
        pos: 0,
        data: Some(Vec::with_capacity(sizing.size)),
        size: 0,
    };
    writer.value();
    Doc {
        data: writer.data.unwrap(),
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
        let mut found = None;
        for i in 0..self.count(at) {
            let (k, value) = self.text(self.child(at, i));
            if k == key.as_bytes() {
                found = Some(value);
            }
        }
        found.unwrap()
    }
    fn number(&self, at: usize) -> f64 {
        assert_eq!(self.data[at], 3);
        std::str::from_utf8(self.text(at + 1).0)
            .unwrap()
            .parse()
            .unwrap()
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
