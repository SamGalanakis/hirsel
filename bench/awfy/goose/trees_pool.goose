// AWFY JavaScript storage.js. Each document owns its descriptors and child slots.
// A TreeRef is a one-based descriptor index, with zero for an empty leaf slot.
namespace trees_pool;
import small;

struct TreeRef { index: u32 = 0 }
struct Tree { start: u32, count: u32 }

root struct Document {
    trees: Tree[>..] = [],
    slots: TreeRef[>..] = []
}

recursive fn build(doc: Document&, depth: i64, random: small::Random&) -> TreeRef {
    let count = if depth == 1 { random.next() % 10 + 1 } else { 4 };
    let start = doc.slots.len;
    let handle = TreeRef { index: (doc.trees.len + 1) as u32 };
    doc.trees.push(Tree { start: start as u32, count: count as u32 });
    doc.slots.resize(start + count, TreeRef {});
    if depth > 1 {
        for slot in start..start + count {
            let child = build(doc, depth - 1, random);
            doc.slots[slot] = child;
        }
    }
    return handle;
}

fn benchmark(depth: i64) -> i64 {
    var doc = Document { .. };
    var random = small::Random {};
    let root = build(doc, depth, random);
    return if doc.trees[root.index - 1].count == 4 { doc.trees.len } else { 0 };
}
