mod program;
#[path = "../support.rs"]
mod support;
use std::collections::HashSet;
use std::sync::Arc;
use support::*;
type Root = Option<Arc<Node>>;
struct Node {
    key: u32,
    value: u64,
    left: Root,
    right: Root,
}
#[derive(Default)]
struct Store;
impl Store {
    fn node(&self, key: u32, value: u64, left: Root, right: Root) -> Arc<Node> {
        Arc::new(Node {
            key,
            value,
            left,
            right,
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
    fn live(&self, roots: &[&Root]) -> usize {
        // A node already seen heads a subtree already seen: versions share it.
        fn mark(r: &Root, seen: &mut HashSet<*const Node>) {
            if let Some(n) = r
                && seen.insert(Arc::as_ptr(n))
            {
                mark(&n.left, seen);
                mark(&n.right, seen);
            }
        }
        let mut seen = HashSet::new();
        roots.iter().for_each(|r| mark(r, &mut seen));
        seen.len()
    }
}
fn main() {
    program::run::<Store>();
}
