// AWFY storage.js. A document owns one descriptor array and one child-slot array.
use crate::som::Random;

#[derive(Clone, Copy)]
struct TreeRef(u32);

struct Tree {
    start: u32,
    count: u32,
}

#[derive(Default)]
struct Document {
    trees: Vec<Tree>,
    slots: Vec<TreeRef>,
}

fn build(doc: &mut Document, depth: u32, random: &mut Random) -> TreeRef {
    let count = if depth == 1 {
        random.next() % 10 + 1
    } else {
        4
    } as u32;
    let start = doc.slots.len();
    let handle = TreeRef((doc.trees.len() + 1) as u32);
    doc.trees.push(Tree {
        start: start as u32,
        count,
    });
    doc.slots.resize(start + count as usize, TreeRef(0));
    if depth > 1 {
        for slot in start..start + count as usize {
            let child = build(doc, depth - 1, random);
            doc.slots[slot] = child;
        }
    }
    handle
}

pub fn benchmark() -> usize {
    let mut doc = Document::default();
    let root = build(&mut doc, 7, &mut Random::new());
    if doc.trees[(root.0 - 1) as usize].count == 4 {
        doc.trees.len()
    } else {
        0
    }
}
