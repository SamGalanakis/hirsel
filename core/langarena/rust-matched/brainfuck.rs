// Reviewed-by: langs-text-921 2026-11-13; Array jump table/30000-byte tape and recursive Vec<Op>/one-byte tape match parsing, loops and wrapping output; compile with overflow checks.
// Matches brainfuck.hirsel: array jump table or recursive Vec<Op>; fresh byte tape.
#[allow(dead_code)]
mod support;
enum Op {
    Dec,
    Inc,
    Prev,
    Next,
    Print,
    Loop(Vec<Op>),
}
struct Tape {
    data: Vec<u8>,
    pos: usize,
}
impl Tape {
    fn advance(&mut self) {
        self.pos += 1;
        if self.pos == self.data.len() {
            self.data.push(0);
        }
    }
}
fn array_run(source: &str) -> u32 {
    let mut code = Vec::new();
    for c in source.bytes() {
        if matches!(c, b'+' | b'-' | b'<' | b'>' | b'[' | b']' | b'.' | b',') {
            code.push(c);
        }
    }
    if code.is_empty() {
        return 0;
    }
    let mut jumps = vec![0usize; code.len()];
    let mut stack = Vec::new();
    for i in 0..code.len() {
        if code[i] == b'[' {
            stack.push(i);
        } else if code[i] == b']' {
            let Some(start) = stack.pop() else {
                return 0;
            };
            jumps[start] = i;
            jumps[i] = start;
        }
    }
    if !stack.is_empty() {
        return 0;
    }
    let mut t = Tape {
        data: vec![0; 30000],
        pos: 0,
    };
    let mut pc = 0;
    let mut result = 0u32;
    while pc < code.len() {
        match code[pc] {
            b'+' => t.data[t.pos] = t.data[t.pos].wrapping_add(1),
            b'-' => t.data[t.pos] = t.data[t.pos].wrapping_sub(1),
            b'>' => t.advance(),
            b'<' => {
                if t.pos > 0 {
                    t.pos -= 1;
                }
            }
            b'[' => {
                if t.data[t.pos] == 0 {
                    pc = jumps[pc];
                }
            }
            b']' => {
                if t.data[t.pos] != 0 {
                    pc = jumps[pc];
                }
            }
            b'.' => result = (result << 2).wrapping_add(u32::from(t.data[t.pos])),
            _ => return 0,
        }
        pc += 1;
    }
    result
}
fn parse(b: &[u8], at: &mut usize) -> Vec<Op> {
    let mut ops = Vec::new();
    while *at < b.len() {
        let c = b[*at];
        *at += 1;
        let op = match c {
            b']' => break,
            b'-' => Op::Dec,
            b'+' => Op::Inc,
            b'<' => Op::Prev,
            b'>' => Op::Next,
            b'.' => Op::Print,
            b'[' => Op::Loop(parse(b, at)),
            _ => continue,
        };
        ops.push(op);
    }
    ops
}
fn execute(ops: &[Op], t: &mut Tape, result: &mut i64) {
    for op in ops {
        match op {
            Op::Dec => t.data[t.pos] = t.data[t.pos].wrapping_sub(1),
            Op::Inc => t.data[t.pos] = t.data[t.pos].wrapping_add(1),
            Op::Prev => {
                if t.pos > 0 {
                    t.pos -= 1;
                }
            }
            Op::Next => t.advance(),
            Op::Print => *result = (*result << 2).wrapping_add(i64::from(t.data[t.pos])),
            Op::Loop(inner) => {
                while t.data[t.pos] != 0 {
                    execute(inner, t, result);
                }
            }
        }
    }
}
fn recursive_run(source: &str) -> u32 {
    let mut at = 0;
    let ops = parse(source.as_bytes(), &mut at);
    let mut t = Tape {
        data: vec![0],
        pos: 0,
    };
    let mut result = 0i64;
    execute(&ops, &mut t, &mut result);
    result as u32
}
fn run(c: &support::Config) -> u32 {
    let recursive = c.name == "Brainfuck::Recursion";
    assert!(recursive || c.name == "Brainfuck::Array");
    for _ in 0..c.warmup {
        let _ = if recursive {
            recursive_run(c.text("warmup_program"))
        } else {
            array_run(c.text("warmup_program"))
        };
    }
    let mut total = 0u32;
    for _ in 0..c.iterations {
        let result = if recursive {
            recursive_run(c.text("program"))
        } else {
            array_run(c.text("program"))
        };
        total = total.wrapping_add(result);
    }
    total
}
fn main() {
    support::main(run);
}
