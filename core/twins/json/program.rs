use crate::support::*;
pub enum Value<N> {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    Array(Vec<N>),
    Object(Vec<(String, N)>),
}
pub trait Document: Default {
    type Node;
    fn node(&mut self, v: Value<Self::Node>) -> Self::Node;
    fn pretty(&self, n: &Self::Node) -> String;
    fn query(&self, n: &Self::Node, path: &[&str]) -> Result<String>;
}
struct Parser<'a, 'b, D: Document> {
    src: &'a [u8],
    at: usize,
    nodes: usize,
    document: usize,
    admission: &'b Admission<'b>,
    store: &'b mut D,
}
impl<D: Document> Parser<'_, '_, D> {
    fn ws(&mut self) {
        while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }
    fn take(&mut self, b: u8) -> Result<()> {
        self.ws();
        if self.src.get(self.at) != Some(&b) {
            return Err("Syntax".into());
        }
        self.at += 1;
        Ok(())
    }
    fn hex(&mut self) -> Result<u16> {
        let end = self.at + 4;
        let s = std::str::from_utf8(self.src.get(self.at..end).ok_or("Syntax")?)
            .map_err(|_| "Syntax")?;
        let n = u16::from_str_radix(s, 16).map_err(|_| "Syntax")?;
        self.at = end;
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        self.take(b'"')?;
        let mut out = String::new();
        let mut begin = self.at;
        loop {
            let b = *self.src.get(self.at).ok_or("Syntax")?;
            if b == b'"' || b == b'\\' {
                out.push_str(std::str::from_utf8(&self.src[begin..self.at]).map_err(|_| "Syntax")?);
                self.at += 1;
                if b == b'"' {
                    return Ok(out);
                }
                let esc = *self.src.get(self.at).ok_or("Syntax")?;
                self.at += 1;
                out.push(match esc {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let a = self.hex()?;
                        let cp = if (0xd800..=0xdbff).contains(&a) {
                            self.take(b'\\')?;
                            self.take(b'u')?;
                            let b = self.hex()?;
                            if !(0xdc00..=0xdfff).contains(&b) {
                                return Err("Syntax".into());
                            }
                            0x10000 + ((u32::from(a) - 0xd800) << 10) + u32::from(b) - 0xdc00
                        } else {
                            u32::from(a)
                        };
                        char::from_u32(cp).ok_or("Syntax")?
                    }
                    _ => return Err("Syntax".into()),
                });
                begin = self.at;
            } else {
                if b < 32 {
                    return Err("Syntax".into());
                }
                self.at += 1;
            }
        }
    }
    fn number(&mut self) -> Result<String> {
        let begin = self.at;
        if self.src.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        match self.src.get(self.at) {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                while self.src.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
            }
            _ => return Err("Syntax".into()),
        }
        if self.src.get(self.at) == Some(&b'.') {
            self.at += 1;
            let start = self.at;
            while self.src.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if start == self.at {
                return Err("Syntax".into());
            }
        }
        if self
            .src
            .get(self.at)
            .is_some_and(|b| *b == b'e' || *b == b'E')
        {
            self.at += 1;
            if self
                .src
                .get(self.at)
                .is_some_and(|b| *b == b'+' || *b == b'-')
            {
                self.at += 1;
            }
            let start = self.at;
            while self.src.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if start == self.at {
                return Err("Syntax".into());
            }
        }
        let s = std::str::from_utf8(&self.src[begin..self.at]).unwrap();
        if !s.parse::<f64>().map_err(|_| "Syntax")?.is_finite() {
            return Err("InvalidNumber".into());
        }
        Ok(s.into())
    }
    fn value(&mut self) -> Result<D::Node> {
        self.ws();
        self.nodes += 1;
        self.admission
            .at(&format!("document{}.node{}", self.document, self.nodes))?;
        let v = match self.src.get(self.at) {
            Some(b'"') => Value::Text(self.string()?),
            Some(b'[') => {
                self.at += 1;
                self.ws();
                let mut xs = Vec::new();
                if self.src.get(self.at) != Some(&b']') {
                    loop {
                        xs.push(self.value()?);
                        self.ws();
                        if self.src.get(self.at) != Some(&b',') {
                            break;
                        }
                        self.at += 1;
                    }
                }
                self.take(b']')?;
                Value::Array(xs)
            }
            Some(b'{') => {
                self.at += 1;
                self.ws();
                let mut xs: Vec<(String, D::Node)> = Vec::new();
                if self.src.get(self.at) != Some(&b'}') {
                    loop {
                        let k = self.string()?;
                        self.take(b':')?;
                        let n = self.value()?;
                        if let Some(i) = xs.iter().position(|(key, _)| key == &k) {
                            xs[i].1 = n;
                        } else {
                            xs.push((k, n));
                        }
                        self.ws();
                        if self.src.get(self.at) != Some(&b',') {
                            break;
                        }
                        self.at += 1;
                    }
                }
                self.take(b'}')?;
                Value::Object(xs)
            }
            Some(b'n') => {
                if self.src.get(self.at..self.at + 4) != Some(b"null") {
                    return Err("Syntax".into());
                }
                self.at += 4;
                Value::Null
            }
            Some(b't') => {
                if self.src.get(self.at..self.at + 4) != Some(b"true") {
                    return Err("Syntax".into());
                }
                self.at += 4;
                Value::Bool(true)
            }
            Some(b'f') => {
                if self.src.get(self.at..self.at + 5) != Some(b"false") {
                    return Err("Syntax".into());
                }
                self.at += 5;
                Value::Bool(false)
            }
            Some(b'-' | b'0'..=b'9') => Value::Number(self.number()?),
            _ => return Err("Syntax".into()),
        };
        Ok(self.store.node(v))
    }
}
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
pub fn aggregate(open: char, close: char, items: Vec<String>, depth: usize) -> String {
    if items.is_empty() {
        return format!("{open}{close}");
    }
    format!(
        "{open}\n{}\n{}{close}",
        items
            .into_iter()
            .map(|s| format!("{}{s}", "  ".repeat(depth + 1)))
            .collect::<Vec<_>>()
            .join(",\n"),
        "  ".repeat(depth)
    )
}
pub fn run<D: Document>() {
    let (input, variant) = input();
    let mut lines = input.lines();
    let path: Vec<_> = lines.next().unwrap().split('/').skip(1).collect();
    let admission = Admission::new(&variant);
    let trace = Trace::default();
    let mut documents = 0;
    let mut bytes = 0;
    let mut checksum = 0;
    let result = (|| {
        for (i, line) in lines.enumerate() {
            let _region = Resource::new(format!("document{}", i + 1), &trace);
            let mut store = D::default();
            let parsed = (|| {
                let mut p = Parser {
                    src: line.as_bytes(),
                    at: 0,
                    nodes: 0,
                    document: i + 1,
                    admission: &admission,
                    store: &mut store,
                };
                let root = p.value()?;
                p.ws();
                if p.at != p.src.len() {
                    return Err("Syntax".into());
                }
                Ok(root)
            })();
            let root = parsed.map_err(|e: String| {
                if e.starts_with("Capacity") {
                    e
                } else {
                    format!("{e}@document{}", i + 1)
                }
            })?;
            let pretty = store.pretty(&root);
            let query = store.query(&root, &path)?;
            println!("query{}={query}", i + 1);
            bytes += pretty.len();
            checksum += sum(pretty.as_bytes());
            documents += 1;
            if variant == "print" {
                println!("{pretty}");
            }
        }
        Ok(())
    })();
    println!("documents={documents} pretty-bytes={bytes} sum={checksum}");
    trace.print();
    finish(result);
}
