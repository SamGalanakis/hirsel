import cli;

struct Score { checksum: i64, flips: i64 }

fn factorial(n: i64) -> i64 {
    var product = 1;
    for i in 2..n + 1 { product *= i; }
    return product;
}

thread_fn chunk(n: i64, first: i64, last: i64) {
    var perm: i64[16] = default<i64[16]>();
    var count: i64[16] = default<i64[16]>();
    for i in n { perm[i] = i; }
    var rank = first;
    for offset in 1..n {
        let i = n - offset;
        let fact = factorial(i);
        let digit = rank / fact;
        rank %= fact;
        count[i] = digit;
        let previous = perm;
        for j in i + 1 { perm[j] = previous[(j + digit) % (i + 1)]; }
    }
    var score = Score { 0, 0 };
    for index in first..last {
        var work = perm;
        var flips = 0;
        while work[0] != 0 {
            let end = work[0];
            for i in (end + 1) / 2 {
                let value = work[i];
                work[i] = work[end - i];
                work[end - i] = value;
            }
            flips++;
        }
        score.flips = max(score.flips, flips);
        if index % 2 == 0 { score.checksum += flips; }
        else { score.checksum -= flips; }
        if index + 1 < last {
            var i = 1;
            loop {
                let first_value = perm[0];
                for j in i { perm[j] = perm[j + 1]; }
                perm[i] = first_value;
                count[i]++;
                if count[i] <= i { break; }
                count[i] = 0;
                i++;
            }
        }
    }
    qput(score);
}

fn main() {
    let n = number(1);
    let workers = number(2);
    if n > 16 { abort("n must be at most sixteen"); }
    let total = factorial(n);
    var ids: i64[>..] = [];
    for i in workers {
        ids.push(thread_spawn(chunk, n, total * i / workers, total * (i + 1) / workers));
    }
    var result = Score { 0, 0 };
    for i in workers {
        let partial = qget<Score>();
        result.checksum += partial.checksum;
        result.flips = max(result.flips, partial.flips);
    }
    for id in ids { thread_wait(id); }
    print(result.checksum);
    print("Pfannkuchen(", n, ") = ", result.flips);
}
