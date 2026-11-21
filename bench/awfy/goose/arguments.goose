namespace arguments;
import std;
import os;

fn read() -> u8[:], i64, i64 {
    let argv = args();
    if argv.len < 4 || argv.len > 5 { abort("usage: harness BENCHMARK OUTER INNER [INPUT]"); }
    let outer, valid_outer = parse_int(argv[2]);
    let inner, valid_inner = parse_int(argv[3]);
    if !valid_outer || !valid_inner || outer <= 0 || inner <= 0 {
        abort("expected positive iteration counts");
    }
    return argv[1], outer, inner;
}

fn input() -> u8[:] {
    let argv = args();
    return if argv.len == 5 { argv[4] } else { argv[1] };
}

fn next_number(input: u8[:], at: i64&, limit: i64) -> i64 {
    let start = at;
    while at < input.len && input[at] != 44 { at += 1; }
    let n, valid = parse_int(input[start..at]);
    at = if at == input.len { 0 } else { at + 1 };
    return if valid && n > 0 && n <= limit { n } else { 0 };
}
