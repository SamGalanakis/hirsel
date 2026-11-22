//! PROG-3: the text the run writes for `main`'s `Error(e)`. A [`Show`] is built
//! from `E`'s source declarations, placed by its layout recipes, and stored in the
//! executable. The native runtime and the evaluator render the same text from it
//! by reading the value's bytes, before the result is cleaned.
//!
//! The text is the value as source would build it: a record `Name(field: v, ..)`,
//! a variant case `Case` or `Case(v, ..)` (`Case(field: v, ..)` when labelled), a
//! tuple `(v, ..)`, unit `()`, a list or array `[v, ..]`, a `Box[T]` as its `T`, a
//! string or char as a quoted literal with `\\ \" \' \n \r \t \0` and `\u{..}`
//! escapes, a bool, an integer in decimal, a float in its shortest round-trip
//! decimal. A value that has no plain text is a fixed [`Node::Text`]: a `Secret`
//! is `[secret]` (TLS-5), any other capability, handle, function, map, `Rc`,
//! `Arc`, compact or opaque value is its type name in angle brackets.

use crate::{Scalar, Tag, case_of};

/// A value's text program: its nodes and the root's offset from the address
/// the run passes (the entry's `Result`).
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Show {
    pub nodes: Vec<Node>,
    pub root: u32,
    pub offset: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Node {
    /// Fixed text, reading nothing.
    Text(String),
    Scalar(Scalar),
    /// A `String` carrier: data pointer, then length.
    Str,
    /// A `List[T]` carrier: data pointer, then length; elements `stride` apart.
    List {
        element: u32,
        stride: u64,
    },
    /// An inline array of `count` elements `stride` apart.
    Array {
        element: u32,
        stride: u64,
        count: u64,
    },
    /// A pointer to the value's block.
    Boxed(u32),
    /// `name(parts)`; a tuple or unit has an empty name.
    Record {
        name: String,
        parts: Vec<Part>,
    },
    /// A variant; with no tag word, its one case.
    Variant {
        tag: Option<Tag>,
        cases: Vec<Case>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Part {
    /// Written `label: value` when present.
    pub label: Option<String>,
    pub offset: u64,
    pub node: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Case {
    pub name: String,
    /// Parentheses even with no parts (a labelled case, `Invalid()`).
    pub parens: bool,
    pub parts: Vec<Part>,
}

/// Byte reads of the run's memory at the value's addresses.
pub trait Memory {
    fn read(&self, at: u64, out: &mut [u8]);
}

enum Step<'a> {
    Value(u32, u64),
    Lit(&'a str),
}

impl Show {
    /// The text of the value at `base + offset`. Iterative: a deep value does
    /// not use the caller's stack.
    pub fn render(&self, base: u64, memory: &impl Memory) -> String {
        let word = |at: u64| {
            let mut bytes = [0; 8];
            memory.read(at, &mut bytes);
            u64::from_le_bytes(bytes)
        };
        let mut out = String::new();
        let mut steps = vec![Step::Value(self.root, base.wrapping_add(self.offset))];
        while let Some(step) = steps.pop() {
            let (node, at) = match step {
                Step::Lit(text) => {
                    out.push_str(text);
                    continue;
                }
                Step::Value(node, at) => (&self.nodes[node as usize], at),
            };
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Scalar(s) => scalar(*s, at, memory, &mut out),
                Node::Str => {
                    let len = word(at + 8);
                    let mut bytes = vec![0; len as usize];
                    if len != 0 {
                        memory.read(word(at), &mut bytes);
                    }
                    quote(&String::from_utf8_lossy(&bytes), '"', &mut out);
                }
                Node::List { element, stride } => {
                    let (data, len) = (word(at), word(at + 8));
                    sequence(&mut steps, *element, data, *stride, len);
                }
                Node::Array {
                    element,
                    stride,
                    count,
                } => sequence(&mut steps, *element, at, *stride, *count),
                Node::Boxed(inner) => steps.push(Step::Value(*inner, word(at))),
                Node::Record { name, parts } => {
                    out.push_str(name);
                    group(&mut steps, at, parts, true);
                }
                Node::Variant { tag, cases } => {
                    let case = tag.map_or(0, |tag| {
                        let mut bytes = [0; 8];
                        memory.read(at + tag.offset, &mut bytes[..tag.width as usize]);
                        case_of(&tag, u64::from_le_bytes(bytes))
                    });
                    let case = &cases[case as usize];
                    out.push_str(&case.name);
                    group(&mut steps, at, &case.parts, case.parens);
                }
            }
        }
        out
    }

    /// The bytes the executable holds; [`Show::decode`] reads them back.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        put(&mut w, u64::from(self.root));
        put(&mut w, self.offset);
        put(&mut w, self.nodes.len() as u64);
        for node in &self.nodes {
            match node {
                Node::Text(text) => {
                    w.push(0);
                    text_put(&mut w, text);
                }
                Node::Scalar(s) => {
                    w.push(1);
                    w.push(Scalar::ALL.iter().position(|x| x == s).unwrap() as u8);
                }
                Node::Str => w.push(2),
                Node::List { element, stride } => {
                    w.push(3);
                    put(&mut w, u64::from(*element));
                    put(&mut w, *stride);
                }
                Node::Array {
                    element,
                    stride,
                    count,
                } => {
                    w.push(4);
                    put(&mut w, u64::from(*element));
                    put(&mut w, *stride);
                    put(&mut w, *count);
                }
                Node::Boxed(inner) => {
                    w.push(5);
                    put(&mut w, u64::from(*inner));
                }
                Node::Record { name, parts } => {
                    w.push(6);
                    text_put(&mut w, name);
                    parts_put(&mut w, parts);
                }
                Node::Variant { tag, cases } => {
                    w.push(7);
                    match tag {
                        None => w.push(0),
                        Some(tag) => {
                            w.push(1);
                            put(&mut w, tag.offset);
                            put(&mut w, tag.width);
                            match tag.encoding {
                                crate::Encoding::Direct => w.push(1),
                                crate::Encoding::Niche {
                                    dataful,
                                    first,
                                    last,
                                    start,
                                } => {
                                    w.push(2);
                                    put(&mut w, u64::from(dataful));
                                    put(&mut w, u64::from(first));
                                    put(&mut w, u64::from(last));
                                    put(&mut w, start);
                                }
                            }
                        }
                    }
                    put(&mut w, cases.len() as u64);
                    for case in cases {
                        text_put(&mut w, &case.name);
                        w.push(u8::from(case.parens));
                        parts_put(&mut w, &case.parts);
                    }
                }
            }
        }
        w
    }

    /// The program [`Show::encode`] wrote, or `None` for other bytes.
    pub fn decode(bytes: &[u8]) -> Option<Show> {
        let mut r = Reader(bytes);
        let root = u32::try_from(r.word()?).ok()?;
        let offset = r.word()?;
        let count = r.word()?;
        let mut nodes = Vec::new();
        for _ in 0..count {
            nodes.push(match r.byte()? {
                0 => Node::Text(r.text()?),
                1 => Node::Scalar(*Scalar::ALL.get(r.byte()? as usize)?),
                2 => Node::Str,
                3 => Node::List {
                    element: r.index()?,
                    stride: r.word()?,
                },
                4 => Node::Array {
                    element: r.index()?,
                    stride: r.word()?,
                    count: r.word()?,
                },
                5 => Node::Boxed(r.index()?),
                6 => Node::Record {
                    name: r.text()?,
                    parts: r.parts()?,
                },
                7 => {
                    let tag = if r.byte()? == 0 {
                        None
                    } else {
                        let offset = r.word()?;
                        let width = r.word()?;
                        let encoding = match r.byte()? {
                            1 => crate::Encoding::Direct,
                            2 => crate::Encoding::Niche {
                                dataful: u32::try_from(r.word()?).ok()?,
                                first: u32::try_from(r.word()?).ok()?,
                                last: u32::try_from(r.word()?).ok()?,
                                start: r.word()?,
                            },
                            _ => return None,
                        };
                        Some(Tag {
                            offset,
                            width,
                            encoding,
                        })
                    };
                    let mut cases = Vec::new();
                    for _ in 0..r.word()? {
                        cases.push(Case {
                            name: r.text()?,
                            parens: r.byte()? != 0,
                            parts: r.parts()?,
                        });
                    }
                    Node::Variant { tag, cases }
                }
                _ => return None,
            });
        }
        r.0.is_empty().then_some(Show {
            nodes,
            root,
            offset,
        })
    }
}

fn sequence(steps: &mut Vec<Step<'_>>, element: u32, data: u64, stride: u64, len: u64) {
    steps.push(Step::Lit("]"));
    for i in (0..len).rev() {
        steps.push(Step::Value(element, data + i * stride));
        if i > 0 {
            steps.push(Step::Lit(", "));
        }
    }
    steps.push(Step::Lit("["));
}

fn group<'a>(steps: &mut Vec<Step<'a>>, at: u64, parts: &'a [Part], parens: bool) {
    if !parens && parts.is_empty() {
        return;
    }
    steps.push(Step::Lit(")"));
    for (i, part) in parts.iter().enumerate().rev() {
        steps.push(Step::Value(part.node, at + part.offset));
        if let Some(label) = &part.label {
            steps.push(Step::Lit(": "));
            steps.push(Step::Lit(label));
        }
        if i > 0 {
            steps.push(Step::Lit(", "));
        }
    }
    steps.push(Step::Lit("("));
}

fn scalar(s: Scalar, at: u64, memory: &impl Memory, out: &mut String) {
    use std::fmt::Write;
    let mut bytes = [0; 8];
    memory.read(at, &mut bytes[..s.size() as usize]);
    let w = u64::from_le_bytes(bytes);
    let _ = match s {
        Scalar::Bool => write!(out, "{}", w != 0),
        Scalar::Char => {
            let c = char::from_u32(w as u32).unwrap_or(char::REPLACEMENT_CHARACTER);
            quote(c.encode_utf8(&mut [0; 4]), '\'', out);
            Ok(())
        }
        Scalar::I8 => write!(out, "{}", w as u8 as i8),
        Scalar::I16 => write!(out, "{}", w as u16 as i16),
        Scalar::I32 => write!(out, "{}", w as u32 as i32),
        Scalar::I64 => write!(out, "{}", w as i64),
        Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64 => write!(out, "{w}"),
        Scalar::F32 => write!(out, "{:?}", f32::from_bits(w as u32)),
        Scalar::F64 => write!(out, "{:?}", f64::from_bits(w)),
    };
}

/// A string or char literal of `text` (syntax.md `escape`).
fn quote(text: &str, delimiter: char, out: &mut String) {
    use std::fmt::Write;
    out.push(delimiter);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c == delimiter => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(delimiter);
}

fn put(w: &mut Vec<u8>, n: u64) {
    w.extend_from_slice(&n.to_le_bytes());
}
fn text_put(w: &mut Vec<u8>, text: &str) {
    put(w, text.len() as u64);
    w.extend_from_slice(text.as_bytes());
}
fn parts_put(w: &mut Vec<u8>, parts: &[Part]) {
    put(w, parts.len() as u64);
    for part in parts {
        match &part.label {
            Some(label) => {
                w.push(1);
                text_put(w, label);
            }
            None => w.push(0),
        }
        put(w, part.offset);
        put(w, u64::from(part.node));
    }
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }
    fn word(&mut self) -> Option<u64> {
        let (head, rest) = self.0.split_first_chunk::<8>()?;
        self.0 = rest;
        Some(u64::from_le_bytes(*head))
    }
    fn index(&mut self) -> Option<u32> {
        u32::try_from(self.word()?).ok()
    }
    fn text(&mut self) -> Option<String> {
        let len = usize::try_from(self.word()?).ok()?;
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        String::from_utf8(head.to_vec()).ok()
    }
    fn parts(&mut self) -> Option<Vec<Part>> {
        let mut parts = Vec::new();
        for _ in 0..self.word()? {
            let label = match self.byte()? {
                0 => None,
                1 => Some(self.text()?),
                _ => return None,
            };
            parts.push(Part {
                label,
                offset: self.word()?,
                node: self.index()?,
            });
        }
        Some(parts)
    }
}
