// Matched to core/programs/m1/generics/matched.hirsel. Independent review pending.
#[path = "../matched_support.rs"]
mod support;
use support::*;
#[derive(Clone, Copy)]
struct Count {
    lo: u32,
    hi: u32,
    count: u64,
}
#[derive(Default)]
struct Counts {
    storage: String,
    entries: Vec<Count>,
    buckets: Vec<u64>,
}
fn hash(s: &str) -> u64 {
    s.bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)))
}
impl Counts {
    fn tally(&mut self, s: &str) {
        if (self.entries.len() + 1) * 2 >= self.buckets.len() {
            let next = if self.buckets.len() < 8 {
                16
            } else {
                self.buckets.len() * 2
            };
            let mut buckets = Vec::with_capacity(next);
            buckets.resize(next, 0u64);
            let mut i = 0u64;
            for e in &self.entries {
                let mut slot = hash(&self.storage[e.lo as usize..e.hi as usize]) % next as u64;
                while buckets[index(slot)] != 0 {
                    slot = (slot + 1) % next as u64;
                }
                buckets[index(slot)] = i + 1;
                i += 1;
            }
            self.buckets = buckets;
        }
        let mut slot = hash(s) % self.buckets.len() as u64;
        while self.buckets[index(slot)] != 0 {
            let i = index(self.buckets[index(slot)] - 1);
            let e = self.entries[i];
            if &self.storage[e.lo as usize..e.hi as usize] == s {
                self.entries[i].count += 1;
                return;
            }
            slot = (slot + 1) % self.buckets.len() as u64;
        }
        let lo = narrow(self.storage.len());
        self.storage.push_str(s);
        let hi = narrow(self.storage.len());
        self.entries.push(Count { lo, hi, count: 1 });
        self.buckets[index(slot)] = self.entries.len() as u64;
    }
}
struct Environment<'a> {
    position: u64,
    variant: &'a str,
}
struct Callback<'a, 'b> {
    call: fn(&mut Environment<'b>, &str) -> Result<String>,
    environment: &'a mut Environment<'b>,
}
fn map(xs: &[String], f: Callback<'_, '_>) -> Result<Vec<String>> {
    let mut ys = Vec::with_capacity(xs.len());
    for x in xs {
        ys.push((f.call)(f.environment, x)?);
    }
    Ok(ys)
}
fn mapped(env: &mut Environment<'_>, s: &str) -> Result<String> {
    env.position += 1;
    let site = name("callback", env.position, ".string");
    admission(env.variant, &site)?;
    if s == "!error" {
        return Err(name("Callback@", env.position, ""));
    }
    let mut t = s.to_owned();
    t.push('!');
    Ok(t)
}
fn count_if(xs: &[String], call: fn(&(), &str) -> bool, env: &()) -> u64 {
    let mut count = 0;
    for x in xs {
        if call(env, x) {
            count += 1;
        }
    }
    count
}
fn run(input: &str, variant: &str, trace: &Trace) -> Result<()> {
    let _list = trace.marker("list");
    let mut xs = Vec::new();
    let mut at = 0;
    while at < input.len() {
        xs.push(line(input, &mut at).to_owned());
    }
    let _mapped = trace.marker("mapped");
    let mut environment = Environment {
        position: 0,
        variant,
    };
    let ys = map(
        &xs,
        Callback {
            call: mapped,
            environment: &mut environment,
        },
    )?;
    let _map = trace.marker("map");
    let mut counts = Counts::default();
    let mut i = 0;
    let mut total = 0usize;
    for s in &ys {
        i += 1;
        total += s.len();
        let site = name("tally", i, ".key");
        admission(variant, &site)?;
        counts.tally(s);
    }
    let mut out = "long=".to_owned();
    num(&mut out, count_if(&ys, |_, s| s.len() >= 6, &()));
    out.push_str(" bytes=");
    num(&mut out, total);
    out.push('\n');
    for e in &counts.entries {
        out.push_str(&counts.storage[e.lo as usize..e.hi as usize]);
        out.push('=');
        num(&mut out, e.count);
        out.push('\n');
    }
    put(&out)
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let trace = Trace::new(6);
    let result = run(&input, &variant, &trace);
    trace.finish(result);
}
