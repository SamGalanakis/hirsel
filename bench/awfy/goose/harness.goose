import std;
import arguments;
import cd;
import deltablue;
import havlak;
import small;
import numeric;
import json_parse;
import richards;
import lists;
import trees;

fn verify_number<F>(inner: i64, input: u8[:], limit: i64) -> bool {
    var at = 0;
    for _ in inner {
        let n = arguments::next_number(input, at, limit);
        if n == 0 || !F(n) { return false; }
    }
    return true;
}

fn json_benchmark(inner: i64, input: u8[:]) -> bool {
    for _ in inner {
        if !json_parse::benchmark(input) { return false; }
    }
    return true;
}

fn run(name: u8[:], inner: i64, input: u8[:]) -> bool {
    if name == "CD" { return cd::benchmark(inner); }
    if name == "Bounce" { return verify_number(inner, input, 50, small::bounce); }
    if name == "DeltaBlue" { return deltablue::benchmark(inner); }
    if name == "Havlak" { return havlak::benchmark(inner); }
    if name == "Json" { return json_benchmark(inner, input); }
    if name == "List" { return verify_number(inner, input, 15, lists::list_benchmark); }
    if name == "Mandelbrot" { return numeric::mandelbrot(inner); }
    if name == "NBody" { return numeric::nbody(inner); }
    if name == "Permute" { return verify_number(inner, input, 6, small::permute); }
    if name == "Queens" { return small::queens(inner, input); }
    if name == "Richards" { return verify_number(inner, input, 10000, richards::benchmark); }
    if name == "Sieve" { return verify_number(inner, input, 5000, small::sieve); }
    if name == "Storage" { return verify_number(inner, input, 7, trees::storage); }
    if name == "Towers" { return verify_number(inner, input, 13, trees::towers); }
    return false;
}

fn main() {
    let name, outer, inner = arguments::read();
    let input = arguments::input();
    for _ in outer {
        if !run(name, inner, input) { abort("benchmark verification failed"); }
        write_stdout("verified\n");
        flush_stdout();
    }
}
