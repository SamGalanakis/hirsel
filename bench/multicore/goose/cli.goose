import std;
import os;

fn number(index: i64) -> i64 {
    var text: u8[>..] = [];
    if index >= arg_count() { abort("missing argument"); }
    arg(index, text);
    let value, ok = parse_int(text);
    if !ok || value < 1 { abort("expected a positive integer"); }
    return value;
}
