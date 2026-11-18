import std;
import arguments;
import json_pool;
import rap;
import trees_pool;

fn run_pool(json: bool, outer: i64, inner: i64) {
    let expected = if json { 156 } else { 5461 };
    for _ in outer {
        var checksum = 0;
        for _ in inner {
            checksum = if json { json_pool::benchmark(rap::document()) } else { trees_pool::benchmark() };
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
    run_pool(name == "JsonPool", outer, inner);
}
