mod program;
#[path = "../support.rs"]
mod support;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::*;
type Root = Option<Arc<Node>>;
struct Node {
    key: u32,
    value: u64,
    left: Root,
    right: Root,
    live: Arc<AtomicUsize>,
}
impl Drop for Node {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Store {
    live: Arc<AtomicUsize>,
}
impl Store {
    fn node(&self, key: u32, value: u64, left: Root, right: Root) -> Arc<Node> {
        self.live.fetch_add(1, Ordering::SeqCst);
        Arc::new(Node {
            key,
            value,
            left,
            right,
            live: self.live.clone(),
        })
    }
    fn build_range(&self, a: u32, b: u32) -> Root {
        if a == b {
            return None;
        }
        let k = a + (b - a) / 2;
        Some(self.node(
            k,
            u64::from(k),
            self.build_range(a, k),
            self.build_range(k + 1, b),
        ))
    }
    fn edit(
        &self,
        r: &Root,
        key: u32,
        value: u64,
        a: &Admission,
        count: &mut usize,
    ) -> Result<Root> {
        let node = r.as_ref().unwrap();
        let (left, right) = if key < node.key {
            (
                self.edit(&node.left, key, value, a, count)?,
                node.right.clone(),
            )
        } else if key > node.key {
            (
                node.left.clone(),
                self.edit(&node.right, key, value, a, count)?,
            )
        } else {
            (node.left.clone(), node.right.clone())
        };
        *count += 1;
        a.at(&format!("draft.node{count}"))?;
        Ok(Some(self.node(
            node.key,
            if key == node.key { value } else { node.value },
            left,
            right,
        )))
    }
}
impl program::Store for Store {
    type Root = Root;
    fn build(&mut self, n: u32) -> Root {
        self.build_range(0, n)
    }
    fn pin(&mut self, r: &Root) -> Root {
        r.clone()
    }
    fn set(&mut self, r: &Root, k: u32, v: u64, a: &Admission) -> Result<Root> {
        self.edit(r, k, v, a, &mut 0)
    }
    fn get(&self, r: &Root, key: u32) -> u64 {
        let mut p = r;
        loop {
            let n = p.as_ref().unwrap();
            if key == n.key {
                return n.value;
            }
            p = if key < n.key { &n.left } else { &n.right };
        }
    }
    fn release(&mut self, r: Root) {
        drop(r);
    }
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}
fn main() {
    program::run::<Store>();
}
