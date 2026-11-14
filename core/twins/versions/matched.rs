// Matched to versions/matched.hirsel: copied nodes, intrusive free links, recursive release/lookup.
// Independent review pending.
#[path = "../matched_support.rs"]
mod support;
use support::*;
type Root = Option<u32>;
#[derive(Clone, Copy)]
struct Node {
    key: u32,
    value: u64,
    left: Root,
    right: Root,
    refs: u64,
}
#[derive(Clone, Copy)]
enum Slot {
    Live(Node),
    Free(Root),
}
#[derive(Default)]
struct Store {
    slots: Vec<Slot>,
    free: Root,
    live: u64,
}
impl Store {
    fn read(&self, h: u32) -> Node {
        match self.slots[h as usize] {
            Slot::Live(n) => n,
            Slot::Free(_) => panic!("vacant node"),
        }
    }
    fn retain(&mut self, r: Root) -> Root {
        if let Some(h) = r {
            let mut n = self.read(h);
            n.refs += 1;
            self.slots[h as usize] = Slot::Live(n);
        }
        r
    }
    fn release(&mut self, r: Root) {
        if let Some(h) = r {
            let mut n = self.read(h);
            n.refs -= 1;
            if n.refs > 0 {
                self.slots[h as usize] = Slot::Live(n);
                return;
            }
            self.slots[h as usize] = Slot::Free(self.free);
            self.free = Some(h);
            self.live -= 1;
            self.release(n.left);
            self.release(n.right);
        }
    }
    fn node(&mut self, key: u32, value: u64, left: Root, right: Root) -> Root {
        let n = Node {
            key,
            value,
            left,
            right,
            refs: 1,
        };
        let h = if let Some(i) = self.free {
            let next = match self.slots[i as usize] {
                Slot::Free(n) => n,
                _ => panic!("occupied free node"),
            };
            self.slots[i as usize] = Slot::Live(n);
            self.free = next;
            i
        } else {
            let i = narrow(self.slots.len());
            self.slots.push(Slot::Live(n));
            i
        };
        self.live += 1;
        Some(h)
    }
    fn build(&mut self, lo: u32, hi: u32) -> Root {
        if lo == hi {
            return None;
        }
        let k = lo + (hi - lo) / 2;
        let left = self.build(lo, k);
        let right = self.build(k + 1, hi);
        self.node(k, u64::from(k), left, right)
    }
    fn draft(
        &mut self,
        r: Root,
        key: u32,
        value: u64,
        count: &mut u64,
        variant: &str,
    ) -> Result<Root> {
        let n = self.read(r.unwrap());
        let (left, right) = if key < n.key {
            let x = self.draft(n.left, key, value, count, variant)?;
            (x, self.retain(n.right))
        } else if key > n.key {
            let x = self.draft(n.right, key, value, count, variant)?;
            (self.retain(n.left), x)
        } else {
            (self.retain(n.left), self.retain(n.right))
        };
        *count += 1;
        let site = name("draft.node", *count, "");
        if let Err(e) = admission(variant, &site, 0) {
            self.release(left);
            self.release(right);
            return Err(e);
        }
        Ok(self.node(
            n.key,
            if key == n.key { value } else { n.value },
            left,
            right,
        ))
    }
    fn lookup(&self, r: Root, key: u32) -> u64 {
        let n = self.read(r.unwrap());
        if key == n.key {
            n.value
        } else if key < n.key {
            self.lookup(n.left, key)
        } else {
            self.lookup(n.right, key)
        }
    }
    fn sum(&self, r: Root, count: u32) -> u64 {
        let mut total = 0;
        let mut key = 0;
        while key < count {
            total += self.lookup(r, key);
            key += 1;
        }
        total
    }
}
fn run(input: View<'_>, variant: &str) -> Result<()> {
    let mut at = 0;
    let n = u32::try_from(number(line(input, &mut at))?).unwrap();
    let mut s = Store::default();
    let mut current = s.build(0, n);
    let pin = s.retain(current);
    let mut completed = 0u64;
    let outcome = (|| {
        while at < input.len() {
            let (a, b) = line(input, &mut at).split_once(' ').ok_or("InvalidKey")?;
            let wide = number(a)?;
            if wide >= u64::from(n) {
                return Err("InvalidKey".into());
            }
            let key = u32::try_from(wide).unwrap();
            let mut count = 0;
            let next = s.draft(current, key, number(b)?, &mut count, variant)?;
            let old = current;
            current = next;
            s.release(old);
            completed += 1;
        }
        Ok(())
    })();
    let mut out = name("updates=", completed, " pinned=");
    num(&mut out, s.sum(pin, n));
    out.push_str(" current=");
    num(&mut out, s.sum(current, n));
    out.push_str(" live=");
    num(&mut out, s.live);
    out.push_str("\nafter-unpin=");
    s.release(pin);
    num(&mut out, s.live);
    out.push_str("\nafter-current=");
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
