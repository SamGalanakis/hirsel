mod program;
#[path = "../support.rs"]
mod support;
use support::*;
type Root = Option<u32>;
struct Node {
    key: u32,
    value: u64,
    left: Root,
    right: Root,
    refs: u64,
}
#[derive(Default)]
struct Store {
    nodes: Vec<Option<Node>>,
    free: Vec<u32>,
    live: usize,
}
impl Store {
    fn retain(&mut self, r: Root) -> Root {
        if let Some(i) = r {
            self.nodes[i as usize].as_mut().unwrap().refs += 1;
        }
        r
    }
    fn node(&mut self, key: u32, value: u64, left: Root, right: Root) -> Root {
        let n = Some(Node {
            key,
            value,
            left,
            right,
            refs: 1,
        });
        self.live += 1;
        Some(if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = n;
            i
        } else {
            let i = u32::try_from(self.nodes.len()).unwrap();
            self.nodes.push(n);
            i
        })
    }
    fn release_node(&mut self, r: Root) {
        let mut work = Vec::new();
        if let Some(i) = r {
            work.push(i);
        }
        while let Some(i) = work.pop() {
            let n = self.nodes[i as usize].as_mut().unwrap();
            n.refs -= 1;
            if n.refs != 0 {
                continue;
            }
            let n = self.nodes[i as usize].take().unwrap();
            self.live -= 1;
            self.free.push(i);
            if let Some(c) = n.left {
                work.push(c);
            }
            if let Some(c) = n.right {
                work.push(c);
            }
        }
    }
    fn build_range(&mut self, a: u32, b: u32) -> Root {
        if a == b {
            return None;
        }
        let k = a + (b - a) / 2;
        let l = self.build_range(a, k);
        let r = self.build_range(k + 1, b);
        self.node(k, u64::from(k), l, r)
    }
    fn edit(
        &mut self,
        r: Root,
        key: u32,
        value: u64,
        a: &Admission,
        count: &mut usize,
    ) -> Result<Root> {
        let n = self.nodes[r.unwrap() as usize].as_ref().unwrap();
        let (k, v, l, r) = (n.key, n.value, n.left, n.right);
        let (left, right) = if key < k {
            let x = self.edit(l, key, value, a, count)?;
            (x, self.retain(r))
        } else if key > k {
            let x = self.edit(r, key, value, a, count)?;
            (self.retain(l), x)
        } else {
            (self.retain(l), self.retain(r))
        };
        *count += 1;
        if let Err(e) = a.at(&format!("draft.node{count}")) {
            self.release_node(left);
            self.release_node(right);
            return Err(e);
        }
        Ok(self.node(k, if key == k { value } else { v }, left, right))
    }
}
impl program::Store for Store {
    type Root = Root;
    fn build(&mut self, n: u32) -> Root {
        self.build_range(0, n)
    }
    fn pin(&mut self, r: &Root) -> Root {
        self.retain(*r)
    }
    fn set(&mut self, r: &Root, k: u32, v: u64, a: &Admission) -> Result<Root> {
        self.edit(*r, k, v, a, &mut 0)
    }
    fn get(&self, r: &Root, key: u32) -> u64 {
        let mut p = *r;
        loop {
            let n = self.nodes[p.unwrap() as usize].as_ref().unwrap();
            if key == n.key {
                return n.value;
            }
            p = if key < n.key { n.left } else { n.right };
        }
    }
    fn release(&mut self, r: Root) {
        self.release_node(r);
    }
    fn live(&self) -> usize {
        self.live
    }
}
fn main() {
    program::run::<Store>();
}
