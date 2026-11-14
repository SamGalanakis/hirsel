// Matched to lru/matched.hirsel: staged inline slots, checked narrowing and streaming operations.
// Independent review pending.
#[path = "../matched_map.rs"]
mod matched_map;
#[path = "../matched_support.rs"]
mod support;
use matched_map::Map;
use support::*;
const NONE: u32 = u32::MAX;
struct Slot {
    key: [u8; 32],
    key_len: u32,
    payload: [u8; 128],
    len: u32,
    prev: u32,
    next: u32,
    generation: u64,
}
fn empty() -> Slot {
    Slot {
        key: [0; 32],
        key_len: 0,
        payload: [0; 128],
        len: 0,
        prev: NONE,
        next: NONE,
        generation: 0,
    }
}
struct Cache {
    slots: Vec<Slot>,
    index: Map<u32>,
    head: u32,
    tail: u32,
    first: Option<(u32, u64)>,
}
impl Cache {
    fn key(&self, h: u32) -> String {
        let s = &self.slots[h as usize];
        let mut bytes = Vec::with_capacity(s.key_len as usize);
        let mut j = 0;
        while j < s.key_len as usize {
            bytes.push(s.key[j]);
            j += 1;
        }
        {
            assert!(utf8(&bytes));
            String::from_bytes(bytes)
        }
    }
    fn touch(&mut self, h: u32) {
        if self.head == h {
            return;
        }
        let i = h as usize;
        let p = self.slots[i].prev;
        let n = self.slots[i].next;
        if p != NONE {
            self.slots[p as usize].next = n;
        }
        if n != NONE {
            self.slots[n as usize].prev = p;
        }
        if self.tail == h {
            self.tail = p;
        }
        self.slots[i].prev = NONE;
        self.slots[i].next = self.head;
        if self.head != NONE {
            self.slots[self.head as usize].prev = h;
        }
        self.head = h;
        if self.tail == NONE {
            self.tail = h;
        }
    }
    fn put(&mut self, name: View<'_>, value: View<'_>) -> Result<bool> {
        if name.len() > 32 || value.len() > 128 {
            return Err("Parse@cache-op".into());
        }
        let mut staged = empty();
        let mut j = 0;
        for b in name.bytes() {
            staged.key[j] = b;
            j += 1;
        }
        staged.key_len = narrow(j);
        j = 0;
        for b in value.bytes() {
            staged.payload[j] = b;
            j += 1;
        }
        staged.len = narrow(j);
        if let Some(&h) = self.index.get(name) {
            let i = h as usize;
            self.slots[i].payload = staged.payload;
            self.slots[i].len = staged.len;
            self.touch(h);
            return Ok(false);
        }
        let owned = name.to_owned();
        let evicted = self.slots.len() == 64;
        let h = if evicted {
            let h = self.tail;
            let old_key = self.key(h);
            self.index.remove(&old_key);
            let i = h as usize;
            staged.prev = self.slots[i].prev;
            staged.next = self.slots[i].next;
            staged.generation = self.slots[i].generation + 1;
            self.slots[i] = staged;
            h
        } else {
            let h = narrow(self.slots.len());
            self.slots.push(staged);
            h
        };
        self.index.insert(owned, h);
        self.touch(h);
        if self.first.is_none() {
            self.first = Some((h, self.slots[h as usize].generation));
        }
        Ok(evicted)
    }
}
fn operations(
    c: &mut Cache,
    input: View<'_>,
    variant: &str,
    hits: &mut u64,
    checksum: &mut u64,
    evictions: &mut u64,
) -> Result<()> {
    let mut at = 0;
    let mut puts = 0u64;
    while at < input.len() {
        let row = line(input, &mut at);
        if let Some(rest) = row.strip_prefix("P ") {
            let (name, value) = rest.split_once(" ").ok_or("Parse@cache-op")?;
            puts += 1;
            admission(variant, &name_site(puts), 0)?;
            if c.put(name, value)? {
                *evictions += 1;
            }
        } else if let Some(name) = row.strip_prefix("G ") {
            if let Some(&h) = c.index.get(name) {
                let i = h as usize;
                let mut value = Vec::with_capacity(c.slots[i].len as usize);
                let mut j = 0;
                while j < c.slots[i].len as usize {
                    value.push(c.slots[i].payload[j]);
                    j += 1;
                }
                c.touch(h);
                *hits += 1;
                *checksum += sum(&value);
            }
        } else if row == "S" {
            if let Some((h, generation)) = c.first {
                if c.slots[h as usize].generation != generation {
                    return Err("StaleHandle@first-entry".into());
                }
            }
        } else {
            return Err("Parse@cache-op".into());
        }
    }
    Ok(())
}
fn name_site(puts: u64) -> String {
    name("put", puts, ".payload")
}
fn run(input: View<'_>, variant: &str, trace: &Trace) -> Result<()> {
    let _owner = trace.marker("cache");
    let mut c = Cache {
        slots: Vec::with_capacity(64),
        index: Map::new(),
        head: NONE,
        tail: NONE,
        first: None,
    };
    let (mut hits, mut checksum, mut evictions) = (0, 0, 0);
    let result = operations(
        &mut c,
        input,
        variant,
        &mut hits,
        &mut checksum,
        &mut evictions,
    );
    let mut out = name("hits=", hits, " sum=");
    num(&mut out, checksum);
    out.push_str(" evictions=");
    num(&mut out, evictions);
    out.push_str("\norder=");
    let mut p = c.head;
    let mut first = true;
    while p != NONE {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&c.key(p));
        p = c.slots[p as usize].next;
    }
    out.push('\n');
    put(&out)?;
    result
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let trace = Trace::new(2);
    let result = run(input.view(), &variant, &trace);
    trace.finish(result);
}
