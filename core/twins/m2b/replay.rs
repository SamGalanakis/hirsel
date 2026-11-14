//! Idiomatic twin of core/programs/m2b/replay `bench(100000)`: n calls of `double`
//! on k's 8 little-endian bytes recorded into a `Vec<Entry>`, then all replayed by
//! re-execution and compared with the entry at the cursor. An entry owns copies of
//! the callee's name (Rust has no code identity; `type_name_of_val` stands in), the
//! argument and the outcome (tag 0 plus the result bytes), as the Hirsel journal does.
//! Prints the same checksum: the entry count plus the first outcome byte of each
//! replayed call.
use std::any::type_name_of_val;

struct Entry {
    code: Vec<u8>,
    arg: Vec<u8>,
    outcome: Vec<u8>,
}

struct Journal {
    entries: Vec<Entry>,
    replay: bool,
    at: usize,
}

enum Diverged {
    Missing(usize),
    Code(usize),
    Arg(usize),
    Outcome(usize),
    Extra(usize),
}

fn double(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b);
    out.extend_from_slice(b);
    out
}

impl Journal {
    fn call(&mut self, id: &str, f: fn(&[u8]) -> Vec<u8>, arg: &[u8]) -> Result<Vec<u8>, Diverged> {
        let r = f(arg);
        let mut out = Vec::with_capacity(1 + r.len());
        out.push(0);
        out.extend_from_slice(&r);
        if self.replay {
            let i = self.at;
            let e = self.entries.get(i).ok_or(Diverged::Missing(i))?;
            if e.code != id.as_bytes() {
                return Err(Diverged::Code(i));
            }
            if e.arg != arg {
                return Err(Diverged::Arg(i));
            }
            if e.outcome != out {
                return Err(Diverged::Outcome(i));
            }
            self.at = i + 1;
        } else {
            self.entries.push(Entry {
                code: id.as_bytes().to_vec(),
                arg: arg.to_vec(),
                outcome: out,
            });
        }
        Ok(r)
    }
}

fn bench(n: u64) -> Result<u64, Diverged> {
    let id = type_name_of_val(&double);
    let mut j = Journal {
        entries: Vec::new(),
        replay: false,
        at: 0,
    };
    for k in 0..n {
        j.call(id, double, &k.to_le_bytes())?;
    }
    j.replay = true;
    j.at = 0;
    let mut sum = j.entries.len() as u64;
    for k in 0..n {
        let o = j.call(id, double, &k.to_le_bytes())?;
        sum += u64::from(o[0]);
    }
    if j.at < j.entries.len() {
        return Err(Diverged::Extra(j.at));
    }
    Ok(sum)
}

fn main() {
    match bench(100000) {
        Ok(v) => println!("{v}"),
        Err(d) => {
            let (kind, i) = match d {
                Diverged::Missing(i) => ("Missing", i),
                Diverged::Code(i) => ("Code", i),
                Diverged::Arg(i) => ("Arg", i),
                Diverged::Outcome(i) => ("Outcome", i),
                Diverged::Extra(i) => ("Extra", i),
            };
            eprintln!("diverged: {kind}({i})");
            std::process::exit(1);
        }
    }
}
