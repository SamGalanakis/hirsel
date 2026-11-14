// Matched to rope/matched.hirsel: one output root, intrusive free slots, no rendered validation text.
// Independent review pending.
#[path = "../matched_support.rs"]
mod support;
use support::*;
struct Chunk {
    value: String,
    refs: u64,
}
enum Slot {
    Live(Chunk),
    Free(Option<u32>),
}
#[derive(Clone, Copy)]
struct Piece {
    id: u32,
    lo: u32,
    hi: u32,
}
type Root = Vec<Piece>;
#[derive(Default)]
struct Store {
    slots: Vec<Slot>,
    free: Option<u32>,
    live: u64,
}
impl Store {
    fn chunk(&self, id: u32) -> &Chunk {
        match &self.slots[id as usize] {
            Slot::Live(c) => c,
            _ => panic!("vacant chunk"),
        }
    }
    fn retain(&mut self, p: Piece) -> Piece {
        let Slot::Live(mut c) = std::mem::replace(&mut self.slots[p.id as usize], Slot::Free(None))
        else {
            panic!("vacant chunk")
        };
        c.refs += 1;
        self.slots[p.id as usize] = Slot::Live(c);
        p
    }
    fn release_piece(&mut self, p: Piece) {
        let Slot::Live(mut c) = std::mem::replace(&mut self.slots[p.id as usize], Slot::Free(None))
        else {
            panic!("vacant chunk")
        };
        c.refs -= 1;
        if c.refs == 0 {
            self.slots[p.id as usize] = Slot::Free(self.free);
            self.free = Some(p.id);
            self.live -= 1;
        } else {
            self.slots[p.id as usize] = Slot::Live(c);
        }
    }
    fn release(&mut self, r: Root) {
        for p in &r {
            self.release_piece(*p);
        }
    }
    fn piece(&mut self, value: View<'_>) -> Piece {
        let hi = narrow(value.len());
        let c = Chunk {
            value: value.to_owned(),
            refs: 1,
        };
        let id = if let Some(i) = self.free {
            let next = match self.slots[i as usize] {
                Slot::Free(n) => n,
                _ => panic!("occupied free chunk"),
            };
            self.slots[i as usize] = Slot::Live(c);
            self.free = next;
            i
        } else {
            let i = narrow(self.slots.len());
            self.slots.push(Slot::Live(c));
            i
        };
        self.live += 1;
        Piece { id, lo: 0, hi }
    }
    fn length(r: &Root) -> usize {
        let mut n = 0;
        for p in r {
            n += (p.hi - p.lo) as usize;
        }
        n
    }
    fn boundary(&self, r: &Root, at: usize) -> bool {
        let mut offset = 0;
        for p in r {
            let end = offset + (p.hi - p.lo) as usize;
            if at >= offset && at <= end {
                return self
                    .chunk(p.id)
                    .value
                    .is_char_boundary(at + p.lo as usize - offset);
            }
            offset = end;
        }
        at == offset
    }
    fn range(&mut self, out: &mut Root, r: &Root, lo: usize, hi: usize) {
        let mut offset = 0;
        for p in r {
            let end = offset + (p.hi - p.lo) as usize;
            if lo < end && hi > offset {
                let base = p.lo as usize;
                let a = narrow(base + if lo > offset { lo - offset } else { 0 });
                let b = narrow(base + if hi < end { hi - offset } else { end - offset });
                out.push(self.retain(Piece {
                    id: p.id,
                    lo: a,
                    hi: b,
                }));
            }
            offset = end;
        }
    }
    fn edit(
        &mut self,
        r: &Root,
        pos: usize,
        len: usize,
        replacement: View<'_>,
        variant: &str,
        number: u64,
    ) -> Result<Root> {
        let n = Self::length(r);
        if pos > n || len > n - pos || !self.boundary(r, pos) || !self.boundary(r, pos + len) {
            return Err("InvalidPosition".into());
        }
        let cap = r.len() + 2;
        admission(
            variant,
            &name("edit", number, ".piece"),
            (((cap as u64) * 12 + 7) / 8) * 8,
        )?;
        let mut out = Vec::with_capacity(cap);
        let inserted = if !replacement.is_empty() {
            Some(self.piece(replacement))
        } else {
            None
        };
        self.range(&mut out, r, 0, pos);
        if let Some(p) = inserted {
            out.push(p);
        }
        self.range(&mut out, r, pos + len, n);
        Ok(out)
    }
}
fn run(input: View<'_>, variant: &str) -> Result<()> {
    let mut at = 0;
    let initial = line(input, &mut at);
    let mut s = Store::default();
    let mut current = Vec::with_capacity(1);
    current.push(s.piece(initial));
    let mut history: Vec<Root> = Vec::new();
    let mut edits = 0u64;
    let outcome = (|| {
        while at < input.len() {
            let row = line(input, &mut at);
            if row == "UNDO" {
                let old = history.pop().ok_or("EmptyUndo")?;
                let displaced = std::mem::replace(&mut current, old);
                s.release(displaced);
            } else {
                let (a, rest) = row.split_once(" ").ok_or("InvalidPosition")?;
                let (b, replacement) = rest.split_once(" ").ok_or("InvalidPosition")?;
                history.reserve(history.len() + 1);
                let next = s.edit(
                    &current,
                    index(number(a)?),
                    index(number(b)?),
                    replacement,
                    variant,
                    edits + 1,
                )?;
                history.push(std::mem::replace(&mut current, next));
                edits += 1;
                if history.len() > 64 {
                    s.release(history.remove(0));
                }
            }
        }
        Ok(())
    })();
    let mut checksum = 0;
    for p in &current {
        checksum += sum(&s.chunk(p.id).value.as_bytes()[p.lo as usize..p.hi as usize]);
    }
    let mut out = name("edits=", edits, " undo=");
    num(&mut out, history.len());
    out.push_str(" bytes=");
    num(&mut out, Store::length(&current));
    out.push_str(" sum=");
    num(&mut out, checksum);
    out.push_str(" live=");
    num(&mut out, s.live);
    out.push_str("\nafter-history=");
    while let Some(old) = history.pop() {
        s.release(old);
    }
    num(&mut out, s.live);
    out.push_str("\nafter-text=");
    s.release(current);
    num(&mut out, s.live);
    out.push('\n');
    put(&out)?;
    outcome
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let result = run(input.view(), &variant);
    Trace::new(1).finish(result);
}
