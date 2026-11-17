import cli;
import math;

struct Block { first: i64, values: f64[>..] }

thread_fn multiply_rows(values: f64[>..], transpose: bool, first: i64, last: i64) {
    var rows: f64[>..] = [];
    for row in first..last {
        var sum = 0.0;
        for value, col in values {
            let k = row + col;
            var index = row;
            if transpose { index = col; }
            sum += value / ((k * (k + 1) / 2 + index + 1) as f64);
        }
        rows.push(sum);
    }
    qput(Block { first: first, values: copy(rows) });
}

fn multiply(values: f64[>..], transpose: bool, workers: i64) -> f64[>..] {
    var ids: i64[>..] = [];
    let n = values.len;
    for i in workers {
        ids.push(thread_spawn(multiply_rows, copy(values), transpose, i * n / workers, (i + 1) * n / workers));
    }
    var result: f64[>..] = [];
    result.resize(n, 0.0);
    for i in workers {
        let part = qget<Block>();
        for value, j in part.values { result[part.first + j] = value; }
    }
    for id in ids { thread_wait(id); }
    return result;
}

fn main() {
    let n = number(1);
    let workers = number(2);
    var u: f64[>..] = [];
    u.resize(n, 1.0);
    var v: f64[>..] = [];
    for i in 10 {
        v = multiply(multiply(copy(u), false, workers), true, workers);
        u = multiply(multiply(copy(v), false, workers), true, workers);
    }
    var uv = 0.0;
    var vv = 0.0;
    for i in n {
        uv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    var line: u8[>..] = [];
    format_flt(line, sqrt(uv / vv), 9);
    print(line);
}
