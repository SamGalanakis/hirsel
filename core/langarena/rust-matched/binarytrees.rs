// Reviewed-by: langs-text-921 2026-11-13; Obj boxed nodes match; compile with overflow checks. Arena was rewritten by arena-alloc-1131 on 2026-11-18 to upstream's Vec arena and has not been re-reviewed.
// Matches binarytrees.hirsel Obj: Box nodes with Option children and checked i32 items.
// Arena: upstream's Vec of 12-byte nodes with i32 links, as the port (arena-alloc-1131).
#[allow(dead_code)]
mod support;
struct Node {
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
    item: i32,
}
fn tree(item: i32, depth: i32) -> Box<Node> {
    let mut node = Box::new(Node {
        left: None,
        right: None,
        item,
    });
    if depth > 0 {
        let offset = 1i32 << (depth - 1);
        node.left = Some(tree(item - offset, depth - 1));
        node.right = Some(tree(item + offset, depth - 1));
    }
    node
}
fn sum(node: &Node) -> u32 {
    let mut total = (node.item as u32).wrapping_add(1);
    if let Some(left) = &node.left {
        total = total.wrapping_add(sum(left));
    }
    if let Some(right) = &node.right {
        total = total.wrapping_add(sum(right));
    }
    total
}
struct ArenaNode {
    item: i32,
    left: i32,
    right: i32,
}
fn arena_tree(arena: &mut Vec<ArenaNode>, item: i32, depth: i32) -> i32 {
    let at = arena.len();
    let idx = i32::try_from(at).unwrap();
    arena.push(ArenaNode {
        item,
        left: -1,
        right: -1,
    });
    if depth > 0 {
        let offset = 1i32 << (depth - 1);
        let left = arena_tree(arena, item - offset, depth - 1);
        let right = arena_tree(arena, item + offset, depth - 1);
        arena[at].left = left;
        arena[at].right = right;
    }
    idx
}
fn arena_sum(arena: &[ArenaNode], idx: i32) -> u32 {
    let node = &arena[usize::try_from(idx).unwrap()];
    let mut total = (node.item as u32).wrapping_add(1);
    if node.left >= 0 {
        total = total.wrapping_add(arena_sum(arena, node.left));
    }
    if node.right >= 0 {
        total = total.wrapping_add(arena_sum(arena, node.right));
    }
    total
}
fn run(c: &support::Config) -> u32 {
    assert!(c.name == "Binarytrees::Obj" || c.name == "Binarytrees::Arena");
    let depth = i32::try_from(c.number("depth")).unwrap();
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        if c.name == "Binarytrees::Arena" {
            let mut arena = Vec::new();
            let root = arena_tree(&mut arena, 0, depth);
            total = total.wrapping_add(arena_sum(&arena, root));
        } else {
            total = total.wrapping_add(sum(&tree(0, depth)));
        }
    }
    total
}
fn main() {
    support::main(run);
}
