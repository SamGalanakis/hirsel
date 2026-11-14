//! std.json's packed W=u32 layout and two-pass parser, without temporary JSON trees.
use crate::matched_sort::{Compare, library_sort};
use crate::support::*;
use std::cmp::Ordering;
#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    Capacity(u64, u64),
    Overflow,
}
#[derive(Debug, PartialEq, Eq)]
pub enum JsonError {
    Syntax(usize),
    NotFinite(usize),
    Build(BuildError),
}
type JsonResult<T> = std::result::Result<T, JsonError>;
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn at(&self) -> u8 {
        self.s.get(self.i).copied().unwrap_or(0)
    }
    fn skip(&mut self) {
        while matches!(self.at(), b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }
    fn hex(&mut self) -> JsonResult<u32> {
        let mut n = 0u32;
        for _ in 0..4 {
            let c = self.at();
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(JsonError::Syntax(self.i)),
            };
            n = n * 16 + u32::from(d);
            self.i += 1;
        }
        Ok(n)
    }
    fn unicode(&mut self) -> JsonResult<char> {
        let start = self.i;
        self.i += 2;
        let a = self.hex()?;
        let cp = if (0xd800..=0xdbff).contains(&a) {
            if self.at() != b'\\' || self.s.get(self.i + 1) != Some(&b'u') {
                return Err(JsonError::Syntax(self.i));
            }
            let pair = self.i;
            self.i += 2;
            let b = self.hex()?;
            if !(0xdc00..=0xdfff).contains(&b) {
                return Err(JsonError::Syntax(pair));
            }
            0x10000 + ((a - 0xd800) << 10) + (b - 0xdc00)
        } else {
            a
        };
        char::from_u32(cp).ok_or_else(|| JsonError::Syntax(start))
    }
    fn string_size(&mut self) -> JsonResult<usize> {
        self.i += 1;
        let mut n = 0usize;
        loop {
            let c = self.at();
            if c == b'"' {
                self.i += 1;
                return Ok(n);
            }
            if c < 0x20 {
                return Err(JsonError::Syntax(self.i));
            }
            if c != b'\\' {
                if self.i + 8 <= self.s.len() {
                    let v = u64::from_le_bytes(self.s[self.i..self.i + 8].try_into().unwrap());
                    let q = v ^ 0x2222222222222222;
                    let slash = v ^ 0x5c5c5c5c5c5c5c5c;
                    let mark = ((v.wrapping_sub(0x2020202020202020) & !v)
                        | (q.wrapping_sub(0x0101010101010101) & !q)
                        | (slash.wrapping_sub(0x0101010101010101) & !slash))
                        & 0x8080808080808080;
                    let run = if mark == 0 {
                        8
                    } else {
                        (mark.trailing_zeros() >> 3) as usize
                    };
                    n += run;
                    self.i += run;
                    continue;
                }
                n += 1;
                self.i += 1;
            } else if self.s.get(self.i + 1) == Some(&b'u') {
                n += self.unicode()?.len_utf8();
            } else if self.s.get(self.i + 1).is_some_and(|e| {
                matches!(e, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't')
            }) {
                n += 1;
                self.i += 2;
            } else {
                return Err(JsonError::Syntax(self.i + 1));
            }
        }
    }
    fn number_size(&mut self) -> JsonResult<usize> {
        let start = self.i;
        loop {
            if self.i + 8 <= self.s.len() {
                let v = u64::from_le_bytes(self.s[self.i..self.i + 8].try_into().unwrap());
                let mark = (v.wrapping_add(0x4646464646464646)
                    | v.wrapping_sub(0x3030303030303030))
                    & 0x8080808080808080;
                if mark == 0 {
                    self.i += 8;
                    continue;
                }
                self.i += (mark.trailing_zeros() >> 3) as usize;
            }
            if !matches!(self.at(), b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                break;
            }
            self.i += 1;
        }
        match crate::matched_float::parse(View(self.s).slice(start, self.i).as_bytes()) {
            Ok(_) => {}
            Err(crate::matched_float::ParseError::Empty) => return Err(JsonError::Syntax(start)),
            Err(crate::matched_float::ParseError::Invalid(p)) => {
                return Err(JsonError::Syntax(start + p));
            }
            Err(crate::matched_float::ParseError::OutOfRange) => {
                return Err(JsonError::NotFinite(start));
            }
        }
        Ok(self.i - start)
    }
    fn ends(&mut self, end: u8) -> bool {
        self.skip();
        if self.at() == end {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn after(&mut self, end: u8) -> JsonResult<bool> {
        self.skip();
        if self.at() == end {
            self.i += 1;
            Ok(true)
        } else if self.at() == b',' {
            self.i += 1;
            Ok(false)
        } else {
            Err(JsonError::Syntax(self.i))
        }
    }
    fn size_value(&mut self) -> JsonResult<usize> {
        self.skip();
        match self.at() {
            b'n' | b't' | b'f' => {
                let word = match self.at() {
                    b'n' => "null",
                    b't' => "true",
                    _ => "false",
                };
                if !self.s[self.i..].starts_with(word.as_bytes()) {
                    return Err(JsonError::Syntax(self.i));
                }
                self.i += word.len();
                Ok(1)
            }
            b'"' => Ok(5 + self.string_size()?),
            b'-' | b'0'..=b'9' => Ok(5 + self.number_size()?),
            b'[' => {
                self.i += 1;
                let mut n = 9;
                if self.ends(b']') {
                    return Ok(n);
                }
                loop {
                    n += self.size_value()? + 4;
                    if self.after(b']')? {
                        return Ok(n);
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut n = 9;
                if self.ends(b'}') {
                    return Ok(n);
                }
                loop {
                    self.skip();
                    if self.at() != b'"' {
                        return Err(JsonError::Syntax(self.i));
                    }
                    let k = self.string_size()?;
                    self.skip();
                    if self.at() != b':' {
                        return Err(JsonError::Syntax(self.i));
                    }
                    self.i += 1;
                    n += 4 + k + self.size_value()? + 4;
                    if self.after(b'}')? {
                        return Ok(n);
                    }
                }
            }
            _ => Err(JsonError::Syntax(self.i)),
        }
    }
    fn write_text(&mut self, blob: &mut Blob) {
        let header = blob.bytes.len();
        blob.word(0);
        let start = blob.bytes.len();
        self.i += 1;
        while self.at() != b'"' {
            if self.at() != b'\\' {
                let lo = self.i;
                while self.at() != b'"' && self.at() != b'\\' {
                    self.i += 1;
                }
                blob.bytes
                    .extend_from_slice(View(self.s).slice(lo, self.i).as_bytes());
            } else if self.s[self.i + 1] == b'u' {
                let c = self.unicode().unwrap();
                let mut b = [0u8; 4];
                blob.bytes
                    .extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            } else {
                let c = match self.s[self.i + 1] {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 8,
                    b'f' => 12,
                    b => b,
                };
                blob.bytes.push(c);
                self.i += 2;
            }
        }
        self.i += 1;
        blob.patch(header, narrow(blob.bytes.len() - start));
    }
    fn write_value(&mut self, blob: &mut Blob) {
        self.skip();
        match self.at() {
            b'n' => {
                blob.bytes.push(0);
                self.i += 4;
            }
            b'f' => {
                blob.bytes.push(1);
                self.i += 5;
            }
            b't' => {
                blob.bytes.push(2);
                self.i += 4;
            }
            b'"' => {
                blob.bytes.push(4);
                self.write_text(blob);
            }
            b'[' | b'{' => {
                let object = self.at() == b'{';
                let end = if object { b'}' } else { b']' };
                blob.bytes.push(if object { 6 } else { 5 });
                let header = blob.bytes.len();
                blob.word(0);
                blob.word(0);
                let data = blob.bytes.len();
                let mut n = 0usize;
                self.i += 1;
                self.skip();
                while self.at() != end {
                    if object {
                        self.skip();
                        self.write_text(blob);
                        self.skip();
                        self.i += 1;
                    }
                    self.write_value(blob);
                    n += 1;
                    self.skip();
                    if self.at() == b',' {
                        self.i += 1;
                        self.skip();
                    }
                }
                self.i += 1;
                let len = blob.bytes.len() - data;
                let mut pos = data;
                for _ in 0..n {
                    blob.word(narrow(pos - data));
                    if object {
                        let (_, next) = blob.text(pos);
                        pos = blob.skip(next);
                    } else {
                        pos = blob.skip(pos);
                    }
                }
                blob.patch(header, narrow(n));
                blob.patch(header + 4, narrow(len));
            }
            _ => {
                let lo = self.i;
                while matches!(self.at(), b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                blob.bytes.push(3);
                blob.word(narrow(self.i - lo));
                blob.bytes
                    .extend_from_slice(View(self.s).slice(lo, self.i).as_bytes());
            }
        }
    }
}
pub fn size(s: impl AsRef<[u8]>) -> JsonResult<usize> {
    let s = s.as_ref();
    let mut p = Parser { s, i: 0 };
    let n = p.size_value()?;
    p.skip();
    if p.i != s.len() {
        return Err(JsonError::Syntax(p.i));
    }
    Ok(n)
}
pub struct Blob {
    bytes: Vec<u8>,
}
#[derive(Clone, Copy)]
struct KeyEnv<'a> {
    blob: &'a Blob,
    container: usize,
}
fn by_key(e: &KeyEnv<'_>, x: u32, y: u32) -> Ordering {
    let (kx, _) = e.blob.member(e.container, x);
    let (ky, _) = e.blob.member(e.container, y);
    if kx < ky {
        Ordering::Less
    } else if ky < kx {
        Ordering::Greater
    } else {
        x.cmp(&y)
    }
}
impl Blob {
    pub fn parse(s: impl AsRef<[u8]>) -> JsonResult<Self> {
        let s = s.as_ref();
        let n = size(s)?;
        u32::try_from(n).map_err(|_| JsonError::Build(BuildError::Overflow))?;
        let mut b = Self {
            bytes: Vec::with_capacity(n),
        };
        Parser { s, i: 0 }.write_value(&mut b);
        Ok(b)
    }
    fn word(&mut self, n: u32) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }
    fn patch(&mut self, p: usize, n: u32) {
        self.bytes[p..p + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn read(&self, p: usize) -> u32 {
        u32::from_le_bytes(self.bytes[p..p + 4].try_into().unwrap())
    }
    fn text(&self, p: usize) -> (&[u8], usize) {
        let n = self.read(p) as usize;
        let start = p + 4;
        (&self.bytes[start..start + n], start + n)
    }
    fn skip(&self, p: usize) -> usize {
        match self.bytes[p] {
            0..=2 => p + 1,
            3 | 4 => p + 5 + self.read(p + 1) as usize,
            5 | 6 => p + 9 + self.read(p + 5) as usize + 4 * self.read(p + 1) as usize,
            _ => unreachable!(),
        }
    }
    fn child(&self, p: usize, i: u32) -> usize {
        let data = p + 9;
        let table = data + self.read(p + 5) as usize;
        data + self.read(table + 4 * i as usize) as usize
    }
    fn member(&self, p: usize, i: u32) -> (&[u8], usize) {
        assert!(i < self.read(p + 1), "json ordinal past count");
        self.text(self.child(p, i))
    }
    fn index(&self, p: usize) -> Vec<u32> {
        let n = self.read(p + 1);
        let mut ord = Vec::with_capacity(n as usize);
        for i in 0..n {
            ord.push(i);
        }
        let env = KeyEnv {
            blob: self,
            container: p,
        };
        library_sort(
            &mut ord,
            Compare {
                call: by_key,
                environment: &env,
            },
        );
        ord
    }
    fn above(&self, p: usize, ord: &[u32], k: &[u8]) -> usize {
        let (mut lo, mut hi) = (0, ord.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if k < self.member(p, ord[mid]).0 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }
    fn below(&self, p: usize, ord: &[u32], k: &[u8]) -> usize {
        let (mut lo, mut hi) = (0, ord.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.member(p, ord[mid]).0 < k {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
    pub fn render(&self, out: &mut Vec<u8>, p: usize, depth: usize, pretty: bool) {
        match self.bytes[p] {
            0 => out.extend_from_slice(b"null"),
            1 => out.extend_from_slice(b"false"),
            2 => out.extend_from_slice(b"true"),
            3 => out.extend_from_slice(self.text(p + 1).0),
            4 => quote(out, self.text(p + 1).0),
            5 => {
                out.push(b'[');
                let n = self.read(p + 1);
                let mut pos = p + 9;
                let mut first = true;
                for _ in 0..n {
                    if !first {
                        out.push(b',');
                    }
                    if pretty {
                        out.push(b'\n');
                        indent(out, depth + 1);
                    }
                    first = false;
                    self.render(out, pos, depth + 1, pretty);
                    pos = self.skip(pos);
                }
                if pretty && !first {
                    out.push(b'\n');
                    indent(out, depth);
                }
                out.push(b']');
            }
            6 => {
                let ord = self.index(p);
                let n = self.read(p + 1);
                let mut first = true;
                out.push(b'{');
                for i in 0..n {
                    let (k, _) = self.member(p, i);
                    let first_pos = self.below(p, &ord, k);
                    if i != ord[first_pos] {
                        continue;
                    }
                    let (_, value) = self.member(p, ord[self.above(p, &ord, k) - 1]);
                    if !first {
                        out.push(b',');
                    }
                    if pretty {
                        out.push(b'\n');
                        indent(out, depth + 1);
                    }
                    first = false;
                    quote(out, k);
                    out.extend_from_slice(if pretty { b": " } else { b":" });
                    self.render(out, value, depth + 1, pretty);
                }
                if pretty && !first {
                    out.push(b'\n');
                    indent(out, depth);
                }
                out.push(b'}');
            }
            _ => unreachable!(),
        }
    }
    pub fn query(&self, out: &mut Vec<u8>, p: usize, path: View<'_>, at: usize) -> Result<()> {
        if at > path.len() {
            self.render(out, p, 0, true);
            return Ok(());
        }
        let end = path
            .slice(at, path.len())
            .find("/")
            .map_or(path.len(), |i| at + i);
        let step = path.slice(at, end);
        match self.bytes[p] {
            6 => {
                let ord = self.index(p);
                let hi = self.above(p, &ord, step.as_bytes());
                if hi == 0 {
                    return Err("MissingPath".into());
                }
                let (k, child) = self.member(p, ord[hi - 1]);
                if k != step.as_bytes() {
                    return Err("MissingPath".into());
                }
                let result = self.query(out, child, path, end + 1);
                drop(ord);
                result
            }
            5 => {
                let i = number(step).map_err(|_| "WrongKind")?;
                if i >= u64::from(self.read(p + 1)) {
                    return Err("MissingPath".into());
                }
                self.query(out, self.child(p, u32::try_from(i).unwrap()), path, end + 1)
            }
            _ => Err("WrongKind".into()),
        }
    }
}
fn indent(out: &mut Vec<u8>, depth: usize) {
    for _ in 0..depth {
        out.extend_from_slice(b"  ");
    }
}
fn quote(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let mut at = 0;
    while let Some((scalar, next)) = next_scalar(s, at) {
        at = next;
        let c = scalar as u32;
        match c {
            34 => out.extend_from_slice(b"\\\""),
            92 => out.extend_from_slice(b"\\\\"),
            10 => out.extend_from_slice(b"\\n"),
            13 => out.extend_from_slice(b"\\r"),
            9 => out.extend_from_slice(b"\\t"),
            8 => out.extend_from_slice(b"\\b"),
            12 => out.extend_from_slice(b"\\f"),
            c if c < 32 => {
                out.extend_from_slice(b"\\u");
                hex4(out, u64::from(c));
            }
            _ => encode_bytes(out, c),
        }
    }
    out.push(b'"');
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_preorder_and_duplicate_contract() {
        let s = r#"{"z":1,"a":2,"z":3}"#;
        let blob = Blob::parse(s).unwrap();
        assert_eq!(blob.bytes.len(), 54);
        assert_eq!(size(s).unwrap(), 54);
        let mut out = Vec::new();
        blob.render(&mut out, 0, 0, true);
        assert_eq!(out, b"{\n  \"z\": 3,\n  \"a\": 2\n}");
        out.clear();
        blob.query(&mut out, 0, View::from("/z"), 1).unwrap();
        assert_eq!(out, b"3");
        let s = r#""\ud834\udd1e""#;
        let blob = Blob::parse(s).unwrap();
        assert_eq!(blob.bytes.len(), 9);
        out.clear();
        blob.render(&mut out, 0, 0, true);
        assert_eq!(out, "\"𝄞\"".as_bytes());
        assert_eq!(size("[1,]"), Err(JsonError::Syntax(3)));
        assert_eq!(size("1e999"), Err(JsonError::NotFinite(0)));
        assert_eq!(size(r#""\q""#), Err(JsonError::Syntax(2)));
        assert_eq!(size("\"\\"), Err(JsonError::Syntax(2)));
    }
}
