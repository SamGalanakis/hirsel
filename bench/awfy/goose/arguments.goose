namespace arguments;
import std;
import os;

fn read() -> u8[:], i64, i64 {
    let argv = args();
    if argv.len != 4 { abort("usage: harness BENCHMARK OUTER INNER"); }
    let outer, valid_outer = parse_int(argv[2]);
    let inner, valid_inner = parse_int(argv[3]);
    if !valid_outer || !valid_inner || outer <= 0 || inner <= 0 {
        abort("expected positive iteration counts");
    }
    return argv[1], outer, inner;
}
