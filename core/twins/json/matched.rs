mod program;
#[path = "../support.rs"]
mod support;
use program::{Document, Value};
use support::*;
#[derive(Default)]
struct Blob {
    bytes: Vec<u8>,
}
#[derive(Default)]
struct Size {
    bytes: usize,
}
impl Document for Size {
    type Node = ();
    const NODE_ADMISSIONS: bool = false;
    fn node(&mut self, value: Value<()>) {
        self.bytes += match value {
            Value::Null | Value::Bool(_) => 1,
            Value::Number(s) | Value::Text(s) => 5 + s.len(),
            Value::Array(xs) => 5 + 4 * xs.len(),
            Value::Object(xs) => 5 + xs.iter().map(|(key, _)| 8 + key.len()).sum::<usize>(),
        };
    }
    fn pretty(&self, _: &()) -> String {
        unreachable!("the sizing pass produces no document")
    }
    fn query(&self, _: &(), _: &[&str]) -> Result<String> {
        unreachable!("the sizing pass produces no document")
    }
}
impl Blob {
    fn word(&mut self, n: u32) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }
    fn text(&mut self, s: &str) {
        self.word(u32::try_from(s.len()).unwrap());
        self.bytes.extend_from_slice(s.as_bytes());
    }
    fn read(&self, p: &mut usize) -> u32 {
        let n = u32::from_le_bytes(self.bytes[*p..*p + 4].try_into().unwrap());
        *p += 4;
        n
    }
    fn string<'a>(&'a self, p: &mut usize) -> &'a str {
        let len = self.read(p) as usize;
        let s = std::str::from_utf8(&self.bytes[*p..*p + len]).unwrap();
        *p += len;
        s
    }
    fn print(&self, id: u32, d: usize) -> String {
        let mut p = id as usize;
        let tag = self.bytes[p];
        p += 1;
        match tag {
            0 => "null".into(),
            1 => "false".into(),
            2 => "true".into(),
            3 => self.string(&mut p).into(),
            4 => program::quote(self.string(&mut p)),
            5 => {
                let n = self.read(&mut p);
                let xs = (0..n)
                    .map(|_| {
                        let id = self.read(&mut p);
                        self.print(id, d + 1)
                    })
                    .collect();
                program::aggregate('[', ']', xs, d)
            }
            6 => {
                let n = self.read(&mut p);
                let xs = (0..n)
                    .map(|_| {
                        let key = self.string(&mut p);
                        let id = self.read(&mut p);
                        format!("{}: {}", program::quote(key), self.print(id, d + 1))
                    })
                    .collect();
                program::aggregate('{', '}', xs, d)
            }
            _ => unreachable!(),
        }
    }
}
impl Document for Blob {
    type Node = u32;
    const NODE_ADMISSIONS: bool = false;
    fn prepare(&mut self, input: &str, document: usize, admission: &Admission) -> Result<()> {
        let mut size = Size::default();
        program::parse_into(input, document, admission, &mut size)?;
        admission.at(&format!("document{document}.blob"))?;
        self.bytes = Vec::with_capacity(size.bytes);
        Ok(())
    }
    fn node(&mut self, v: Value<u32>) -> u32 {
        let id = u32::try_from(self.bytes.len()).unwrap();
        match v {
            Value::Null => self.bytes.push(0),
            Value::Bool(b) => self.bytes.push(if b { 2 } else { 1 }),
            Value::Number(s) => {
                self.bytes.push(3);
                self.text(&s);
            }
            Value::Text(s) => {
                self.bytes.push(4);
                self.text(&s);
            }
            Value::Array(xs) => {
                self.bytes.push(5);
                self.word(xs.len() as u32);
                for x in xs {
                    self.word(x);
                }
            }
            Value::Object(xs) => {
                self.bytes.push(6);
                self.word(xs.len() as u32);
                for (k, x) in xs {
                    self.text(&k);
                    self.word(x);
                }
            }
        }
        id
    }
    fn pretty(&self, n: &u32) -> String {
        self.print(*n, 0)
    }
    fn query(&self, n: &u32, path: &[&str]) -> Result<String> {
        let mut id = *n;
        for key in path {
            let mut p = id as usize + 1;
            id = match self.bytes[id as usize] {
                5 => {
                    let index = key.parse::<u32>().map_err(|_| "WrongKind")?;
                    let n = self.read(&mut p);
                    if index >= n {
                        return Err("MissingPath".into());
                    }
                    p += index as usize * 4;
                    self.read(&mut p)
                }
                6 => {
                    let n = self.read(&mut p);
                    let mut found = None;
                    for _ in 0..n {
                        let k = self.string(&mut p);
                        let child = self.read(&mut p);
                        if k == *key {
                            found = Some(child);
                        }
                    }
                    found.ok_or("MissingPath")?
                }
                _ => return Err("WrongKind".into()),
            };
        }
        Ok(self.print(id, 0))
    }
}
fn main() {
    program::run::<Blob>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sized_blob_does_not_grow_during_publication() {
        let admission = Admission::new("normal");
        for input in [
            include_str!("../../corpus/m1/json/inputs/small.txt"),
            include_str!("../../corpus/m1/json/inputs/normal.txt"),
        ] {
            for (i, source) in input.lines().skip(1).enumerate() {
                let mut blob = Blob::default();
                blob.prepare(source, i + 1, &admission).unwrap();
                let reserved = blob.bytes.capacity();
                program::parse_into(source, i + 1, &admission, &mut blob).unwrap();
                assert_eq!(blob.bytes.len(), reserved, "sizing must be exact");
                assert_eq!(blob.bytes.capacity(), reserved, "publication must not grow");
            }
        }
    }
}
