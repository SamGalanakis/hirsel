// Review pending: langs-text-921. No matched ratio before independent review.
// Matches binarytrees.hirsel Obj: Box nodes with Option children and checked i32 items.
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
#[derive(Clone, Copy)]
struct Id {
    pool: u64,
    index: u64,
    generation: u64,
}
enum Slot {
    Occupied(u64, ArenaNode),
    Vacant(u64, u64),
}
struct Pool {
    me: u64,
    slots: Vec<Slot>,
    free: u64,
    live: u64,
}
struct ArenaNode {
    item: i32,
    left: Option<Id>,
    right: Option<Id>,
}
impl Pool {
    fn new() -> Self {
        Self {
            me: 1,
            slots: Vec::new(),
            free: u64::MAX,
            live: 0,
        }
    }
    fn insert(&mut self, node: ArenaNode) -> Id {
        assert_eq!(self.free, u64::MAX);
        let index = u64::try_from(self.slots.len()).unwrap();
        self.slots.push(Slot::Occupied(0, node));
        self.live += 1;
        Id {
            pool: self.me,
            index,
            generation: 0,
        }
    }
    fn get(&self, id: Id) -> &ArenaNode {
        assert_eq!(id.pool, self.me);
        match &self.slots[usize::try_from(id.index).unwrap()] {
            Slot::Occupied(g, n) if *g == id.generation => n,
            _ => panic!("Stale"),
        }
    }
    fn update(&mut self, id: Id, left: Id, right: Id) {
        assert_eq!(id.pool, self.me);
        match &mut self.slots[usize::try_from(id.index).unwrap()] {
            Slot::Occupied(g, n) if *g == id.generation => {
                n.left = Some(left);
                n.right = Some(right);
            }
            _ => panic!("Stale"),
        }
    }
}
fn arena_tree(p: &mut Pool, item: i32, depth: i32) -> Id {
    let id = p.insert(ArenaNode {
        item,
        left: None,
        right: None,
    });
    if depth > 0 {
        let offset = 1i32 << (depth - 1);
        let left = arena_tree(p, item - offset, depth - 1);
        let right = arena_tree(p, item + offset, depth - 1);
        p.update(id, left, right);
    }
    id
}
fn arena_sum(p: &Pool, id: Id) -> u32 {
    let node = p.get(id);
    let mut total = (node.item as u32).wrapping_add(1);
    if let Some(left) = node.left {
        total = total.wrapping_add(arena_sum(p, left));
    }
    if let Some(right) = node.right {
        total = total.wrapping_add(arena_sum(p, right));
    }
    total
}
fn run(c: &support::Config) -> u32 {
    assert!(c.name == "Binarytrees::Obj" || c.name == "Binarytrees::Arena");
    let depth = i32::try_from(c.number("depth")).unwrap();
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        if c.name == "Binarytrees::Arena" {
            let mut p = Pool::new();
            let root = arena_tree(&mut p, 0, depth);
            total = total.wrapping_add(arena_sum(&p, root));
        } else {
            total = total.wrapping_add(sum(&tree(0, depth)));
        }
    }
    total
}
fn main() {
    support::main(run);
}
