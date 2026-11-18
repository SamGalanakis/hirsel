import std;
import os;
import cd;
import deltablue;
import havlak;
import small;
import numeric;
import json_parse;
import rap;
import richards;
import lists;
import trees;

fn verify<F>(inner: i64) -> bool {
    for _ in inner {
        if !F() { return false; }
    }
    return true;
}

fn json_benchmark() -> bool { return json_parse::benchmark(rap::document()); }

fn run(name: u8[:], inner: i64) -> bool {
    if name == "CD" { return cd::benchmark(inner); }
    if name == "Bounce" { return verify(inner, small::bounce); }
    if name == "DeltaBlue" { return deltablue::benchmark(inner); }
    if name == "Havlak" { return havlak::benchmark(inner); }
    if name == "Json" { return verify(inner, json_benchmark); }
    if name == "List" { return verify(inner, lists::list_benchmark); }
    if name == "Mandelbrot" { return numeric::mandelbrot(inner); }
    if name == "NBody" { return numeric::nbody(inner); }
    if name == "Permute" { return verify(inner, small::permute); }
    if name == "Queens" { return verify(inner, small::queens); }
    if name == "Richards" { return verify(inner, richards::benchmark); }
    if name == "Sieve" { return verify(inner, small::sieve); }
    if name == "Storage" { return verify(inner, trees::storage); }
    if name == "Towers" { return verify(inner, trees::towers); }
    return false;
}

fn main() {
    let argv = args();
    if argv.len != 4 { abort("usage: harness BENCHMARK OUTER INNER"); }
    let outer, valid_outer = parse_int(argv[2]);
    let inner, valid_inner = parse_int(argv[3]);
    if !valid_outer || !valid_inner || outer <= 0 || inner <= 0 {
        abort("expected positive iteration counts");
    }
    for _ in outer {
        if !run(argv[1], inner) { abort("benchmark verification failed"); }
        write_stdout("verified\n");
        flush_stdout();
    }
}
