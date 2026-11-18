// AWFY JavaScript json.js. The parser's one document owns the grow-only pools.
// One-based handles stay inside the benchmark; zero ends a chain.
namespace json_pool;

struct NodeRef { index: u32 = 0 }
struct ElementRef { index: u32 = 0 }
struct MemberRef { index: u32 = 0 }
struct ObjectRef { index: u32 = 0 }

struct Text { start: u32, end: u32, decoded: bool }
struct Elements { head: ElementRef, count: u32 }
struct Element { node: NodeRef, next: ElementRef = ElementRef {} }
struct Member { name: Text, node: NodeRef, next: MemberRef = MemberRef {} }
struct Object { head: MemberRef, hash_index: MemberRef[32] }
enum Value {
    Null, True, False,
    Number { raw: Text },
    Str { text: Text },
    Array { elements: Elements },
    ObjectValue { handle: ObjectRef }
}

struct Cursor { index: i64 = 0, line: i64 = 1, column: i64 = 0 }

root struct Document {
    nodes: Value[>..] = [],
    elements: Element[>..] = [],
    members: Member[>..] = [],
    objects: Object[>..] = [],
    decoded: u8[>..] = []
}

fn text_range(start: i64, end: i64, decoded: bool) -> Text {
    return Text { start: start as u32, end: end as u32, decoded: decoded };
}

fn slot(length: i64) -> i64 { return length * 1402589 & 31; }

fn byte(input: const u8[:], c: Cursor&) -> u8 {
    return if c.index < input.len { input[c.index] } else { 0 };
}

fn advance(input: const u8[:], c: Cursor&) {
    if byte(input, c) == 10 {
        c.line += 1;
        c.column = 0;
    }
    c.index += 1;
    c.column += 1;
}

fn skip_whitespace(input: const u8[:], c: Cursor&) {
    while whitespace(byte(input, c)) {
        advance(input, c);
    }
}

fn whitespace(b: u8) -> bool {
    return b == 32 || b == 9 || b == 10 || b == 13;
}

fn consume(input: const u8[:], c: Cursor&, expected: u8) -> bool {
    if byte(input, c) != expected { return false; }
    advance(input, c);
    return true;
}

fn required(input: const u8[:], c: Cursor&, expected: u8) {
    if !consume(input, c, expected) { abort("unexpected JSON byte"); }
}

fn literal(input: const u8[:], c: Cursor&, spelling: const u8[:]) {
    for b in spelling { required(input, c, b); }
}

fn digit(input: const u8[:], c: Cursor&) -> bool {
    let current = byte(input, c);
    if current < 48 || current > 57 { return false; }
    advance(input, c);
    return true;
}

fn read_number(input: const u8[:], c: Cursor&) -> Value {
    let start = c.index;
    consume(input, c, 45);
    let leading_zero = byte(input, c) == 48;
    if !digit(input, c) { abort("expected JSON digit"); }
    if !leading_zero {
        while digit(input, c) {}
    }
    if consume(input, c, 46) {
        if !digit(input, c) { abort("expected JSON fraction"); }
        while digit(input, c) {}
    }
    if consume(input, c, 101) || consume(input, c, 69) {
        if !consume(input, c, 43) { consume(input, c, 45); }
        if !digit(input, c) { abort("expected JSON exponent"); }
        while digit(input, c) {}
    }
    return Value.Number { raw: text_range(start, c.index, false) };
}

fn read_string(doc: Document&, input: const u8[:], c: Cursor&) -> Text {
    required(input, c, 34);
    var start = c.index;
    let decoded_start = doc.decoded.len;
    var decoded = false;
    while byte(input, c) != 34 {
        if byte(input, c) == 92 {
            decoded = true;
            doc.decoded.append(input[start..c.index]);
            advance(input, c);
            let escaped = byte(input, c);
            if escaped == 34 || escaped == 47 || escaped == 92 { doc.decoded.push(escaped); }
            else if escaped == 98 { doc.decoded.push(8); }
            else if escaped == 102 { doc.decoded.push(12); }
            else if escaped == 110 { doc.decoded.push(10); }
            else if escaped == 114 { doc.decoded.push(13); }
            else if escaped == 116 { doc.decoded.push(9); }
            else { abort("invalid JSON escape"); }
            advance(input, c);
            start = c.index;
        } else {
            if byte(input, c) < 32 { abort("invalid JSON string"); }
            c.index += 1;
            c.column += 1;
        }
    }
    var range = text_range(start, c.index, false);
    if decoded {
        doc.decoded.append(input[start..c.index]);
        range = text_range(decoded_start, doc.decoded.len, true);
    }
    advance(input, c);
    return range;
}

recursive fn read_array(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    advance(input, c);
    var head = ElementRef {};
    var tail = ElementRef {};
    var count: u32 = 0;
    skip_whitespace(input, c);
    if !consume(input, c, 93) {
        var more = true;
        while more {
            skip_whitespace(input, c);
            let node = read_node(doc, input, c);
            let next = ElementRef { index: (doc.elements.len + 1) as u32 };
            doc.elements.push(Element { node: node });
            if tail.index == 0 { head = next; }
            else { doc.elements[tail.index - 1].next = next; }
            tail = next;
            count += 1;
            skip_whitespace(input, c);
            more = consume(input, c, 44);
        }
        required(input, c, 93);
    }
    return Value.Array { elements: Elements { head: head, count: count } };
}

recursive fn read_object(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    advance(input, c);
    let none = MemberRef {};
    var object = Object { head: none, hash_index: [none; 32] };
    var tail = none;
    var count: u32 = 0;
    skip_whitespace(input, c);
    if !consume(input, c, 125) {
        var more = true;
        while more {
            skip_whitespace(input, c);
            let name = read_string(doc, input, c);
            skip_whitespace(input, c);
            required(input, c, 58);
            skip_whitespace(input, c);
            let node = read_node(doc, input, c);
            let next = MemberRef { index: (doc.members.len + 1) as u32 };
            doc.members.push(Member { name: name, node: node });
            if tail.index == 0 { object.head = next; }
            else { doc.members[tail.index - 1].next = next; }
            tail = next;
            count += 1;
            object.hash_index[slot(name.end - name.start)] = if count <= 255 { next } else { none };
            skip_whitespace(input, c);
            more = consume(input, c, 44);
        }
        required(input, c, 125);
    }
    let handle = ObjectRef { index: (doc.objects.len + 1) as u32 };
    doc.objects.push(object);
    return Value.ObjectValue { handle: handle };
}

recursive fn read_value(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    let kind = byte(input, c);
    if kind == 110 {
        literal(input, c, "null");
        return Value.Null;
    }
    if kind == 116 {
        literal(input, c, "true");
        return Value.True;
    }
    if kind == 102 {
        literal(input, c, "false");
        return Value.False;
    }
    if kind == 34 { return Value.Str { text: read_string(doc, input, c) }; }
    if kind == 91 { return read_array(doc, input, c); }
    if kind == 123 { return read_object(doc, input, c); }
    return read_number(input, c);
}

recursive fn read_node(doc: Document&, input: const u8[:], c: Cursor&) -> NodeRef {
    let node = read_value(doc, input, c);
    let handle = NodeRef { index: (doc.nodes.len + 1) as u32 };
    doc.nodes.push(node);
    return handle;
}

fn matches(input: const u8[:], decoded: const u8[:], text: Text, key: const u8[:]) -> bool {
    return if text.decoded { decoded[text.start..text.end] == key }
           else { input[text.start..text.end] == key };
}

fn find(doc: Document&, input: const u8[:], object: Object&, key: const u8[:]) -> NodeRef {
    let cached = object.hash_index[slot(key.len)];
    if cached.index != 0 {
        let member .= doc.members[cached.index - 1];
        if matches(input, doc.decoded, member.name, key) { return member.node; }
    }

    // The original hash index is only a hint. The last matching member wins.
    var found = NodeRef {};
    var next = object.head;
    while next.index != 0 {
        let member .= doc.members[next.index - 1];
        if matches(input, doc.decoded, member.name, key) { found = member.node; }
        next = member.next;
    }
    return found;
}

fn checksum(doc: Document&, input: const u8[:], root: NodeRef) -> i64 {
    match doc.nodes[root.index - 1] {
        ObjectValue value => {
            let object .= doc.objects[value.handle.index - 1];
            let head = find(doc, input, object, "head");
            if head.index == 0 { return 0; }
            let head_is_object = match doc.nodes[head.index - 1] { ObjectValue => true, _ => false };
            if !head_is_object { return 0; }
            let operations = find(doc, input, object, "operations");
            if operations.index == 0 { return 0; }
            return match doc.nodes[operations.index - 1] {
                Array a => a.elements.count,
                _ => 0
            };
        },
        _ => { return 0; }
    }
}

fn benchmark(input: const u8[:]) -> i64 {
    // Each entry consumes at least one byte, and escapes never expand the input.
    // The one-based indices and text offsets therefore fit in u32.
    if input.len > 4294967295 { abort("document offsets exceed u32"); }
    var doc = Document { .. };
    var c = Cursor {};
    skip_whitespace(input, c);
    let root = read_node(doc, input, c);
    skip_whitespace(input, c);
    return if c.index == input.len { checksum(doc, input, root) } else { 0 };
}
