// AWFY JavaScript som.js, bounce.js, permute.js, queens.js and sieve.js.
namespace small;
import math;

struct Random { seed: i64 = 74755 }

fn next(random: Random&) -> i64 {
    random.seed = (random.seed * 1309 + 13849) & 65535;
    return random.seed;
}

struct Ball { x: i64, y: i64, vx: i64, vy: i64 }

fn step(ball: Ball&) -> bool {
    var bounced = false;
    ball.x += ball.vx;
    ball.y += ball.vy;
    if ball.x > 500 {
        ball.x = 500;
        ball.vx = -abs(ball.vx);
        bounced = true;
    }
    if ball.x < 0 {
        ball.x = 0;
        ball.vx = abs(ball.vx);
        bounced = true;
    }
    if ball.y > 500 {
        ball.y = 500;
        ball.vy = -abs(ball.vy);
        bounced = true;
    }
    if ball.y < 0 {
        ball.y = 0;
        ball.vy = abs(ball.vy);
        bounced = true;
    }
    return bounced;
}

fn bounce(steps: i64) -> bool {
    var random = Random {};
    var balls: Ball[100] = [Ball { x: 0, y: 0, vx: 0, vy: 0 }; 100];
    for i in balls.len {
        balls[i] = Ball {
            x: random.next() % 500, y: random.next() % 500,
            vx: random.next() % 300 - 150, vy: random.next() % 300 - 150
        };
    }
    var count = 0;
    for _ in steps {
        for i in balls.len {
            if balls[i].step() { count += 1; }
        }
    }
    return count == 1331;
}

struct Permutation { values: i32[6], count: i64 }

fn swap(p: Permutation&, i: i64, j: i64) {
    let saved = p.values[i];
    p.values[i] = p.values[j];
    p.values[j] = saved;
}

recursive fn visit(p: Permutation&, n: i64) {
    p.count += 1;
    if n > 0 {
        let n1 = n - 1;
        p.visit(n1);
        var remaining = n;
        while remaining > 0 {
            remaining -= 1;
            p.swap(n1, remaining);
            p.visit(n1);
            p.swap(n1, remaining);
        }
    }
}

fn permute(size: i64) -> bool {
    var p = Permutation { values: [0; 6], count: 0 };
    p.visit(size);
    return p.count == 8660;
}

struct Board {
    rows: bool[8],
    maxs: bool[16],
    mins: bool[16],
    queen_rows: i64[8]
}

fn available(b: Board&, r: i64, c: i64) -> bool {
    return b.rows[r] && b.maxs[c + r] && b.mins[c - r + 7];
}

fn set_available(b: Board&, r: i64, c: i64, free: bool) {
    b.rows[r] = free;
    b.maxs[c + r] = free;
    b.mins[c - r + 7] = free;
}

recursive fn place_queen(b: Board&, column: i64, size: i64) -> bool {
    for row in size {
        if b.available(row, column) {
            b.queen_rows[row] = column;
            b.set_available(row, column, false);
            if column + 1 == size || b.place_queen(column + 1, size) { return true; }
            b.set_available(row, column, true);
        }
    }
    return false;
}

fn solve_queens(size: i64) -> bool {
    var board = Board { rows: [true; 8], maxs: [true; 16], mins: [true; 16], queen_rows: [-1; 8] };
    return board.place_queen(0, size);
}

fn queens(inner: i64, sizes: const u8[:]) -> bool {
    if sizes.len == 0 { return false; }
    var at = 0;
    for _ in inner {
        var solved = 0;
        for _ in 10 {
            let digit = sizes[at];
            if digit < 52 || digit > 56 { return false; }
            if solve_queens(digit as i64 - 48) { solved += 1; }
            at += 1;
            if at == sizes.len { at = 0; }
        }
        if solved != 10 { return false; }
    }
    return true;
}

fn sieve(size: i64) -> bool {
    var flags: bool[5000] = [true; 5000];
    var count = 0;
    for i in 2..size + 1 {
        if flags[i - 1] {
            count += 1;
            var multiple = i + i;
            while multiple <= size {
                flags[multiple - 1] = false;
                multiple += i;
            }
        }
    }
    return count == 669;
}
