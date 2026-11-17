import cli;
import dictionary;

struct Counts { index: i64, values: i64[16] }
let LENGTHS = [1, 2, 3, 4, 6, 12, 18];
let QUERIES = ["GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT"];

fn base(c: u8) -> i64 {
    let lower = c | 32;
    if lower == 'a' { return 0; }
    if lower == 'c' { return 1; }
    if lower == 'g' { return 2; }
    if lower == 't' { return 3; }
    return -1;
}

fn sequence(file: u8[:]) -> u8[>..] {
    var dna: u8[>..] = [];
    var start = 0;
    var third = false;
    while start < file.len {
        var end = start;
        while end < file.len && file[end] != '\n' { end++; }
        let line = file[start..end];
        if line.len > 0 && line[0] == '>' {
            if third { break; }
            third = line.len >= 6 && line[0..6] == ">THREE";
        } else if third {
            for c in line {
                let digit = base(c);
                if digit >= 0 { dna.push(digit as u8); }
            }
        }
        start = end + 1;
    }
    return dna;
}

fn frequency(table: dictionary<i64, i64>&, key: i64) -> i64 {
    let found = get(table, key);
    if found { return found; }
    return 0;
}

thread_fn tally(bases: u8[>..], first: i64, last: i64) {
    for index in first..last {
        let k = LENGTHS[index];
        var table: dictionary<i64, i64> = dictionary<i64, i64> {};
        for frame in k {
            var at = frame;
            while at + k <= bases.len {
                var key = 0;
                for i in at..at + k { key = (key << 2) | (bases[i] as i64); }
                let value .= get_or_insert(table, key, 0);
                value += 1;
                at += k;
            }
        }
        var counts = default<i64[16]>();
        if index < 2 {
            for key in 1 << (2 * k) { counts[key] = frequency(table, key); }
        } else {
            var key = 0;
            for c in QUERIES[index - 2] { key = (key << 2) | base(c); }
            counts[0] = frequency(table, key);
        }
        qput(Counts { index: index, values: counts });
    }
}

fn frequencies(values: i64[:], k: i64) {
    var total = 0;
    var counts: i64[>..] = [];
    for value in values { total += value; counts.push(value); }
    loop {
        var best = 0;
        for value, i in counts { if value > counts[best] { best = i; } }
        if counts[best] == 0 { break; }
        var line: u8[>..] = [];
        for i in k { line.push("ACGT"[(best >> (2 * (k - i - 1))) & 3]); }
        line.push(' ');
        format_flt(line, (counts[best] as f64) * 100.0 / (total as f64), 3);
        print(line);
        counts[best] = 0;
    }
    print("");
}

fn main() {
    let workers = min(number(1), LENGTHS.len);
    var file: u8[>..] = [];
    read_stdin(file);
    let dna = sequence(file);
    var ids: i64[>..] = [];
    for i in workers {
        ids.push(thread_spawn(tally, copy(dna), i * LENGTHS.len / workers, (i + 1) * LENGTHS.len / workers));
    }
    var results: Counts[>..] = [];
    for i in LENGTHS.len { results.push(qget<Counts>()); }
    for id in ids { thread_wait(id); }
    for index in LENGTHS.len {
        for output in results {
            if output.index == index {
                if index < 2 { frequencies(output.values[0..(1 << (2 * LENGTHS[index]))], LENGTHS[index]); }
                else { print(output.values[0], "\t", QUERIES[index - 2]); }
                break;
            }
        }
    }
}
