// AWFY json.js, using the same document pools and offsets as the Hirsel and Goose ports.
#[derive(Clone, Copy, Default)]
struct NodeRef(u32);
#[derive(Clone, Copy, Default)]
struct ElementRef(u32);
#[derive(Clone, Copy, Default)]
struct MemberRef(u32);
#[derive(Clone, Copy)]
struct ObjectRef(u32);

#[derive(Clone, Copy)]
struct Text {
    start: u32,
    end: u32,
    decoded: bool,
}

struct Elements {
    head: ElementRef,
    count: u32,
}
struct Element {
    node: NodeRef,
    next: ElementRef,
}
struct Member {
    name: Text,
    node: NodeRef,
    next: MemberRef,
}
struct Object {
    head: MemberRef,
    hash_index: [MemberRef; 32],
}
enum Value {
    Null,
    True,
    False,
    Number(Text),
    Str(Text),
    Array(Elements),
    Object(ObjectRef),
}

#[derive(Default)]
struct Document {
    nodes: Vec<Value>,
    elements: Vec<Element>,
    members: Vec<Member>,
    objects: Vec<Object>,
    decoded: Vec<u8>,
}

struct Parser<'a> {
    input: &'a [u8],
    index: usize,
    line: usize,
    column: usize,
    doc: Document,
}

fn slot(length: usize) -> usize {
    length.wrapping_mul(1402589) & 31
}

impl Parser<'_> {
    fn byte(&self) -> u8 {
        self.input.get(self.index).copied().unwrap_or(0)
    }

    fn advance(&mut self) {
        if self.byte() == b'\n' {
            self.line += 1;
            self.column = 0;
        }
        self.index += 1;
        self.column += 1;
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.byte(), b' ' | b'\t' | b'\r' | b'\n') {
            self.advance();
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.byte() != expected {
            return false;
        }
        self.advance();
        true
    }

    fn required(&mut self, expected: u8) {
        assert!(
            self.consume(expected),
            "unexpected byte at {}:{}",
            self.line,
            self.column
        );
    }

    fn literal(&mut self, spelling: &[u8]) {
        for &byte in spelling {
            self.required(byte);
        }
    }

    fn digit(&mut self) -> bool {
        if !self.byte().is_ascii_digit() {
            return false;
        }
        self.advance();
        true
    }

    fn read_number(&mut self) -> Value {
        let start = self.index;
        self.consume(b'-');
        let leading_zero = self.byte() == b'0';
        assert!(self.digit(), "expected JSON digit");
        if !leading_zero {
            while self.digit() {}
        }
        if self.consume(b'.') {
            assert!(self.digit(), "expected JSON fraction");
            while self.digit() {}
        }
        if self.consume(b'e') || self.consume(b'E') {
            if !self.consume(b'+') {
                self.consume(b'-');
            }
            assert!(self.digit(), "expected JSON exponent");
            while self.digit() {}
        }
        Value::Number(Text {
            start: start as u32,
            end: self.index as u32,
            decoded: false,
        })
    }

    fn read_string(&mut self) -> Text {
        self.required(b'"');
        let mut start = self.index;
        let decoded_start = self.doc.decoded.len();
        let mut decoded = false;
        while self.byte() != b'"' {
            if self.byte() == b'\\' {
                decoded = true;
                self.doc
                    .decoded
                    .extend_from_slice(&self.input[start..self.index]);
                self.advance();
                let byte = match self.byte() {
                    byte @ (b'"' | b'/' | b'\\') => byte,
                    b'b' => 8,
                    b'f' => 12,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    _ => panic!("invalid JSON escape"),
                };
                self.doc.decoded.push(byte);
                self.advance();
                start = self.index;
            } else {
                assert!(self.byte() >= 32, "invalid JSON string");
                self.index += 1;
                self.column += 1;
            }
        }
        let end = self.index;
        self.advance();
        if decoded {
            self.doc.decoded.extend_from_slice(&self.input[start..end]);
            Text {
                start: decoded_start as u32,
                end: self.doc.decoded.len() as u32,
                decoded,
            }
        } else {
            Text {
                start: start as u32,
                end: end as u32,
                decoded,
            }
        }
    }

    fn read_array(&mut self) -> Value {
        self.advance();
        let mut head = ElementRef::default();
        let mut tail = ElementRef::default();
        let mut count = 0;
        self.skip_whitespace();
        if !self.consume(b']') {
            loop {
                self.skip_whitespace();
                let node = self.read_node();
                let next = ElementRef((self.doc.elements.len() + 1) as u32);
                self.doc.elements.push(Element {
                    node,
                    next: ElementRef::default(),
                });
                if tail.0 == 0 {
                    head = next;
                } else {
                    self.doc.elements[(tail.0 - 1) as usize].next = next;
                }
                tail = next;
                count += 1;
                self.skip_whitespace();
                if !self.consume(b',') {
                    break;
                }
            }
            self.required(b']');
        }
        Value::Array(Elements { head, count })
    }

    fn read_object(&mut self) -> Value {
        self.advance();
        let none = MemberRef::default();
        let mut object = Object {
            head: none,
            hash_index: [none; 32],
        };
        let mut tail = none;
        let mut count = 0;
        self.skip_whitespace();
        if !self.consume(b'}') {
            loop {
                self.skip_whitespace();
                let name = self.read_string();
                self.skip_whitespace();
                self.required(b':');
                self.skip_whitespace();
                let node = self.read_node();
                let next = MemberRef((self.doc.members.len() + 1) as u32);
                self.doc.members.push(Member {
                    name,
                    node,
                    next: none,
                });
                if tail.0 == 0 {
                    object.head = next;
                } else {
                    self.doc.members[(tail.0 - 1) as usize].next = next;
                }
                tail = next;
                count += 1;
                object.hash_index[slot((name.end - name.start) as usize)] =
                    if count <= 255 { next } else { none };
                self.skip_whitespace();
                if !self.consume(b',') {
                    break;
                }
            }
            self.required(b'}');
        }
        let handle = ObjectRef((self.doc.objects.len() + 1) as u32);
        self.doc.objects.push(object);
        Value::Object(handle)
    }

    fn read_node(&mut self) -> NodeRef {
        let value = match self.byte() {
            b'n' => {
                self.literal(b"null");
                Value::Null
            }
            b't' => {
                self.literal(b"true");
                Value::True
            }
            b'f' => {
                self.literal(b"false");
                Value::False
            }
            b'"' => Value::Str(self.read_string()),
            b'[' => self.read_array(),
            b'{' => self.read_object(),
            _ => self.read_number(),
        };
        let handle = NodeRef((self.doc.nodes.len() + 1) as u32);
        self.doc.nodes.push(value);
        handle
    }

    fn matches(&self, text: Text, key: &[u8]) -> bool {
        let bytes = if text.decoded {
            &self.doc.decoded
        } else {
            self.input
        };
        &bytes[text.start as usize..text.end as usize] == key
    }

    fn find(&self, object: &Object, key: &[u8]) -> Option<&Value> {
        let cached = object.hash_index[slot(key.len())];
        if cached.0 != 0 {
            let member = &self.doc.members[(cached.0 - 1) as usize];
            if self.matches(member.name, key) {
                return Some(&self.doc.nodes[(member.node.0 - 1) as usize]);
            }
        }
        let mut found = None;
        let mut next = object.head;
        while next.0 != 0 {
            let member = &self.doc.members[(next.0 - 1) as usize];
            if self.matches(member.name, key) {
                found = Some(&self.doc.nodes[(member.node.0 - 1) as usize]);
            }
            next = member.next;
        }
        found
    }

    fn checksum(&self, root: NodeRef) -> usize {
        let Value::Object(handle) = self.doc.nodes[(root.0 - 1) as usize] else {
            return 0;
        };
        let object = &self.doc.objects[(handle.0 - 1) as usize];
        if !matches!(self.find(object, b"head"), Some(Value::Object(_))) {
            return 0;
        }
        match self.find(object, b"operations") {
            Some(Value::Array(elements)) => elements.count as usize,
            _ => 0,
        }
    }
}

pub fn benchmark() -> usize {
    let input = include_bytes!("rap.json");
    let mut parser = Parser {
        input,
        index: 0,
        line: 1,
        column: 1,
        doc: Document::default(),
    };
    parser.skip_whitespace();
    let root = parser.read_node();
    parser.skip_whitespace();
    if parser.index == input.len() {
        parser.checksum(root)
    } else {
        0
    }
}
