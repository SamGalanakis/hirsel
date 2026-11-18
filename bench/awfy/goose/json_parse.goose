// AWFY JavaScript json.js. References index grow-only pools, as in LangArena.
// Each object retains the original 32-slot hash index and member order.
namespace json_parse;
import std;

struct Text { start: i64, end: i64 }
struct Sequence { head: i64 = -1, tail: i64 = -1, count: i64 = 0 }
struct Element { node: i64, next: i64 = -1 }
struct Member { name: Text, node: i64 }
struct Object { members: Sequence, hash_index: i64[32] }
enum Value {
    Null, True, False,
    Number { raw: Text },
    Str { text: Text },
    Array { elements: Sequence },
    ObjectValue { index: i64 }
}

struct Cursor {
    index: i64 = 0,
    line: i64 = 1,
    column: i64 = 0,
    capture_start: i64 = 0,
    capture_offset: i64 = 0
}

root struct Document {
    nodes: Value[>..] = [],
    elements: Element[>..] = [],
    members: Member[>..] = [],
    objects: Object[>..] = [],
    text: u8[>..] = []
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
    var current = byte(input, c);
    while current == 32 || current == 9 || current == 10 || current == 13 {
        advance(input, c);
        current = byte(input, c);
    }
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

fn begin_capture(doc: Document&, c: Cursor&) {
    c.capture_offset = doc.text.len;
    c.capture_start = c.index;
}

fn capture_piece(doc: Document&, input: const u8[:], c: Cursor&) {
    doc.text.append(input[c.capture_start..c.index]);
}

fn finish_capture(doc: Document&, input: const u8[:], c: Cursor&) -> Text {
    capture_piece(doc, input, c);
    return Text { start: c.capture_offset, end: doc.text.len };
}

fn read_number(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    begin_capture(doc, c);
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
    return Value.Number { raw: finish_capture(doc, input, c) };
}

fn read_string(doc: Document&, input: const u8[:], c: Cursor&) -> Text {
    required(input, c, 34);
    begin_capture(doc, c);
    while byte(input, c) != 34 {
        let ch = byte(input, c);
        if ch == 92 {
            capture_piece(doc, input, c);
            advance(input, c);
            let escaped = byte(input, c);
            if escaped == 34 || escaped == 47 || escaped == 92 { doc.text.push(escaped); }
            else if escaped == 98 { doc.text.push(8); }
            else if escaped == 102 { doc.text.push(12); }
            else if escaped == 110 { doc.text.push(10); }
            else if escaped == 114 { doc.text.push(13); }
            else if escaped == 116 { doc.text.push(9); }
            else { abort("invalid JSON escape"); }
            advance(input, c);
            c.capture_start = c.index;
        } else {
            if ch < 32 { abort("invalid JSON string"); }
            advance(input, c);
        }
    }
    let result = finish_capture(doc, input, c);
    advance(input, c);
    return result;
}

fn add(doc: Document&, list: Sequence&, index: i64) -> i64 {
    let element = doc.elements.len;
    doc.elements.push(Element { node: index });
    if list.head < 0 { list.head = element; }
    else { doc.elements[list.tail].next = element; }
    list.tail = element;
    list.count += 1;
    return element;
}

recursive fn read_array(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    advance(input, c);
    var elements = Sequence {};
    skip_whitespace(input, c);
    if !consume(input, c, 93) {
        var more = true;
        while more {
            skip_whitespace(input, c);
            let index = read_node(doc, input, c);
            add(doc, elements, index);
            skip_whitespace(input, c);
            more = consume(input, c, 44);
        }
        required(input, c, 93);
    }
    return Value.Array { elements: elements };
}

recursive fn read_object(doc: Document&, input: const u8[:], c: Cursor&) -> Value {
    advance(input, c);
    var object = Object { members: Sequence {}, hash_index: [0; 32] };
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
            let index = doc.members.len;
            doc.members.push(Member { name: name, node: node });
            let entry = add(doc, object.members, index);
            object.hash_index[slot(name.end - name.start)] = if object.members.count <= 255 { entry + 1 } else { 0 };
            skip_whitespace(input, c);
            more = consume(input, c, 44);
        }
        required(input, c, 125);
    }
    let index = doc.objects.len;
    doc.objects.push(object);
    return Value.ObjectValue { index: index };
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
    return read_number(doc, input, c);
}

recursive fn read_node(doc: Document&, input: const u8[:], c: Cursor&) -> i64 {
    let node = read_value(doc, input, c);
    let index = doc.nodes.len;
    doc.nodes.push(node);
    return index;
}

fn get(doc: Document&, index: i64, name: const u8[:]) -> i64 {
    let entry = doc.objects[index].hash_index[slot(name.len)];
    if entry == 0 { return -1; }
    let m = doc.members[doc.elements[entry - 1].node];
    return if doc.text[m.name.start..m.name.end] == name { m.node } else { -1 };
}

fn verify_object(doc: Document&, index: i64) -> bool {
    let head = get(doc, index, "head");
    let operations = get(doc, index, "operations");
    if head < 0 || operations < 0 { return false; }
    let head_is_object = match doc.nodes[head] { ObjectValue => true, _ => false };
    let valid_operations = match doc.nodes[operations] {
        Array a => a.elements.count == 156,
        _ => false
    };
    return head_is_object && valid_operations;
}

fn benchmark(input: const u8[:]) -> bool {
    var doc = Document { .. };
    var c = Cursor {};
    skip_whitespace(input, c);
    let root = read_node(doc, input, c);
    skip_whitespace(input, c);
    if c.index != input.len { return false; }
    return match doc.nodes[root] {
        ObjectValue o => verify_object(doc, o.index),
        _ => false
    };
}
