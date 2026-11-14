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
fn hash_bytes(s: &[u8]) -> u64 {
    s.iter()
        .copied()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)))
}
impl Counts {
    fn tally(&mut self, s: &String) {
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
                let mut slot =
                    hash_bytes(&self.storage[e.lo as usize..e.hi as usize]) % next as u64;
                while buckets[index(slot)] != 0 {
                    slot = (slot + 1) % next as u64;
                }
                buckets[index(slot)] = i + 1;
                i += 1;
            }
            self.buckets = buckets;
        }
        let mut slot = hash_bytes(s.as_bytes()) % self.buckets.len() as u64;
        while self.buckets[index(slot)] != 0 {
            let i = index(self.buckets[index(slot)] - 1);
            let e = self.entries[i];
            if &self.storage[e.lo as usize..e.hi as usize] == s.as_bytes() {
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
    call: fn(&mut Environment<'b>, &String) -> Result<String>,
    environment: &'a mut Environment<'b>,
}
fn map(xs: &[String], f: Callback<'_, '_>) -> Result<Vec<String>> {
    let mut ys = Vec::with_capacity(xs.len());
    for x in xs {
        ys.push((f.call)(f.environment, x)?);
    }
    Ok(ys)
}
fn mapped(env: &mut Environment<'_>, s: &String) -> Result<String> {
    env.position += 1;
    let site = name("callback", env.position, ".string");
    admission(env.variant, &site, (s.len() as u64) + 1)?;
    if s == "!error" {
        return Err(name("Callback@", env.position, "").into());
    }
    let mut t = String::copy(s);
    t.push('!');
    Ok(t)
}
fn count_if(xs: &[String], call: fn(&(), &String) -> bool, env: &()) -> u64 {
    let mut count = 0;
    for x in xs {
        if call(env, x) {
            count += 1;
        }
    }
    count
}
fn run(input: View<'_>, variant: &str, trace: &Trace) -> Result<()> {
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
        admission(variant, &site, s.len() as u64)?;
        counts.tally(s);
    }
    let mut out = String::new();
    out.push_str("long=");
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
    let result = run(input.view(), &variant, &trace);
    trace.finish(result);
}
