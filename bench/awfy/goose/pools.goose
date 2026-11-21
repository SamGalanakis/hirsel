import std;
import arguments;
import json_pool;
import trees_pool;

fn run_pool(json: bool, outer: i64, inner: i64, input: u8[:]) {
    let expected = if json { 156 } else { 5461 };
    for _ in outer {
        var checksum = 0;
        var at = 0;
        for _ in inner {
            if json {
                checksum = json_pool::benchmark(input);
            } else {
                let depth = arguments::next_number(input, at, 7);
                if depth == 0 {
                    checksum = 0;
                    break;
                }
                checksum = trees_pool::benchmark(depth);
            }
            if checksum != expected { break; }
        }
        let verified = checksum == expected;
        print(if verified { "verified checksum=" } else { "failed checksum=" }, checksum);
        flush_stdout();
        if !verified { exit(1); }
    }
}

fn main() {
    let name, outer, inner = arguments::read();
    if name != "JsonPool" && name != "StoragePool" { abort("unknown pool benchmark"); }
    run_pool(name == "JsonPool", outer, inner, arguments::input());
}
