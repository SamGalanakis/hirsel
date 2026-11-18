// AWFY havlak.js and som.js. Graph, forest and finder each own one link pool.
const NONE: usize = usize::MAX;
const UNVISITED: usize = 2147483647;
const MAX_NON_BACK_PREDS: usize = 32768;

#[derive(Clone, Copy)]
struct Chain {
    head: usize,
    tail: usize,
    count: usize,
}

impl Default for Chain {
    fn default() -> Self {
        Self {
            head: NONE,
            tail: NONE,
            count: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Link {
    value: usize,
    next: usize,
}

impl Chain {
    fn append(&mut self, links: &mut Vec<Link>, value: usize) {
        let index = links.len();
        links.push(Link { value, next: NONE });
        if self.head == NONE {
            self.head = index;
        } else {
            links[self.tail].next = index;
        }
        self.tail = index;
        self.count += 1;
    }

    fn add_unique(&mut self, links: &mut Vec<Link>, value: usize) {
        let mut edge = self.head;
        while edge != NONE {
            if links[edge].value == value {
                return;
            }
            edge = links[edge].next;
        }
        self.append(links, value);
    }
}

#[derive(Default)]
struct Block {
    incoming: Chain,
    outgoing: Chain,
    present: bool,
}

#[derive(Default)]
struct Graph {
    blocks: Vec<Block>,
    edges: Vec<(usize, usize)>,
    links: Vec<Link>,
}

impl Graph {
    fn create_node(&mut self, name: usize) {
        while self.blocks.len() <= name {
            self.blocks.push(Block::default());
        }
        self.blocks[name].present = true;
    }

    fn add_edge(&mut self, from: usize, to: usize) {
        self.create_node(from);
        self.create_node(to);
        self.blocks[from].outgoing.append(&mut self.links, to);
        self.blocks[to].incoming.append(&mut self.links, from);
        self.edges.push((from, to));
    }

    fn straight(&mut self, from: usize, count: usize) -> usize {
        for i in 0..count {
            self.add_edge(from + i, from + i + 1);
        }
        from + count
    }

    fn diamond(&mut self, start: usize) -> usize {
        self.add_edge(start, start + 1);
        self.add_edge(start, start + 2);
        self.add_edge(start + 1, start + 3);
        self.add_edge(start + 2, start + 3);
        start + 3
    }

    fn base_loop(&mut self, from: usize) -> usize {
        let header = self.straight(from, 1);
        let first = self.diamond(header);
        let second_start = self.straight(first, 1);
        let second = self.diamond(second_start);
        let footer = self.straight(second, 1);
        self.add_edge(second, second_start);
        self.add_edge(first, header);
        self.add_edge(footer, from);
        self.straight(footer, 1)
    }

    fn build_cfg(&mut self) {
        let mut n = 2;
        for _ in 0..10 {
            self.create_node(n + 1);
            self.add_edge(2, n + 1);
            n += 1;
            for _ in 0..10 {
                let top = n;
                n = self.straight(n, 1);
                for _ in 0..5 {
                    n = self.base_loop(n);
                }
                let bottom = self.straight(n, 1);
                self.add_edge(n, top);
                n = bottom;
            }
            self.add_edge(n, 1);
        }
    }
}

struct SimpleLoop {
    blocks: Chain,
    children: Chain,
    parent: usize,
    root: bool,
    reducible: bool,
    depth: usize,
    nesting: usize,
}

#[derive(Default)]
struct Forest {
    loops: Vec<SimpleLoop>,
    links: Vec<Link>,
}

impl Forest {
    fn new_loop(&mut self, block: usize, reducible: bool) -> usize {
        let index = self.loops.len();
        self.loops.push(SimpleLoop {
            blocks: Chain::default(),
            children: Chain::default(),
            parent: NONE,
            root: block == NONE,
            reducible,
            depth: 0,
            nesting: 0,
        });
        if block != NONE {
            self.loops[index].blocks.append(&mut self.links, block);
        }
        index
    }

    fn set_parent(&mut self, child: usize, parent: usize) {
        self.loops[child].parent = parent;
        self.loops[parent]
            .children
            .add_unique(&mut self.links, child);
    }

    fn nesting(&mut self, index: usize, depth: usize) {
        self.loops[index].depth = depth;
        let mut edge = self.loops[index].children.head;
        while edge != NONE {
            let child = self.links[edge].value;
            self.nesting(child, depth + 1);
            self.loops[index].nesting =
                self.loops[index].nesting.max(self.loops[child].nesting + 1);
            edge = self.links[edge].next;
        }
    }

    fn nest(&mut self) {
        for index in 0..self.loops.len() {
            if !self.loops[index].root && self.loops[index].parent == NONE {
                self.set_parent(index, 0);
            }
        }
        self.nesting(0, 0);
    }
}

#[derive(Clone, Copy)]
struct Number {
    key: usize,
    hash: usize,
    value: usize,
    next: usize,
}

struct Dictionary {
    buckets: Vec<usize>,
    entries: Vec<Number>,
}

impl Dictionary {
    fn new() -> Self {
        Self {
            buckets: vec![NONE; 16],
            entries: Vec::new(),
        }
    }

    fn get(&self, key: usize) -> usize {
        let hash = key ^ (key >> 16);
        let mut index = self.buckets[hash & (self.buckets.len() - 1)];
        while index != NONE {
            let entry = self.entries[index];
            if entry.key == key && entry.hash == hash {
                return entry.value;
            }
            index = entry.next;
        }
        NONE
    }

    fn put(&mut self, key: usize, value: usize) {
        let hash = key ^ (key >> 16);
        let bucket = hash & (self.buckets.len() - 1);
        let mut index = self.buckets[bucket];
        let mut previous = NONE;
        while index != NONE {
            let entry = self.entries[index];
            if entry.key == key && entry.hash == hash {
                self.entries[index].value = value;
                return;
            }
            previous = index;
            index = entry.next;
        }
        let added = self.entries.len();
        self.entries.push(Number {
            key,
            hash,
            value,
            next: NONE,
        });
        if previous == NONE {
            self.buckets[bucket] = added;
        } else {
            self.entries[previous].next = added;
        }
        if self.entries.len() > self.buckets.len() {
            self.transfer();
        }
    }

    fn transfer(&mut self) {
        let size = self.buckets.len();
        self.buckets.resize(size * 2, NONE);
        for base in 0..size {
            let mut index = std::mem::replace(&mut self.buckets[base], NONE);
            let mut tails = [NONE; 2];
            while index != NONE {
                let entry = self.entries[index];
                let group = usize::from(entry.hash & size != 0);
                if tails[group] == NONE {
                    self.buckets[base + group * size] = index;
                } else {
                    self.entries[tails[group]].next = index;
                }
                tails[group] = index;
                index = entry.next;
            }
            for tail in tails {
                if tail != NONE {
                    self.entries[tail].next = NONE;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Top,
    NonHeader,
    Reducible,
    SelfLoop,
    Irreducible,
    Dead,
}

#[derive(Clone, Copy)]
struct UnionNode {
    parent: usize,
    block: usize,
    loop_id: usize,
    ordinal: usize,
}

struct Frame {
    ordinal: usize,
    edge: usize,
}

struct Finder {
    incoming: Vec<Chain>,
    back: Vec<Chain>,
    links: Vec<Link>,
    numbers: Dictionary,
    header: Vec<usize>,
    class: Vec<Class>,
    last: Vec<usize>,
    nodes: Vec<UnionNode>,
}

impl Finder {
    fn new(size: usize) -> Self {
        Self {
            incoming: vec![Chain::default(); size],
            back: vec![Chain::default(); size],
            links: Vec::new(),
            numbers: Dictionary::new(),
            header: vec![0; size],
            class: vec![Class::Top; size],
            last: vec![0; size],
            nodes: vec![
                UnionNode {
                    parent: 0,
                    block: NONE,
                    loop_id: NONE,
                    ordinal: 0
                };
                size
            ],
        }
    }

    fn enter(&mut self, graph: &Graph, path: &mut Vec<Frame>, block: usize, ordinal: usize) {
        self.nodes[ordinal] = UnionNode {
            parent: ordinal,
            block,
            loop_id: NONE,
            ordinal,
        };
        self.numbers.put(block, ordinal);
        path.push(Frame {
            ordinal,
            edge: graph.blocks[block].outgoing.head,
        });
    }

    fn dfs(&mut self, graph: &Graph) {
        let mut path = Vec::new();
        let mut last = 0;
        self.enter(graph, &mut path, 0, last);
        while let Some(frame) = path.last_mut() {
            if frame.edge == NONE {
                self.last[frame.ordinal] = last;
                path.pop();
            } else {
                let edge = graph.links[frame.edge];
                frame.edge = edge.next;
                if self.numbers.get(edge.value) == UNVISITED {
                    last += 1;
                    self.enter(graph, &mut path, edge.value, last);
                }
            }
        }
    }

    fn ancestor(&self, w: usize, v: usize) -> bool {
        w <= v && v <= self.last[w]
    }

    fn find_set(&mut self, start: usize) -> usize {
        let mut path = Vec::new();
        let mut node = start;
        while node != self.nodes[node].parent {
            let parent = self.nodes[node].parent;
            if parent != self.nodes[parent].parent {
                path.push(node);
            }
            node = parent;
        }
        let parent = self.nodes[start].parent;
        for index in path {
            self.nodes[index].parent = parent;
        }
        node
    }

    fn identify_edges(&mut self, graph: &Graph) {
        for w in 0..self.nodes.len() {
            self.header[w] = 0;
            self.class[w] = Class::NonHeader;
            let block = self.nodes[w].block;
            if block == NONE {
                self.class[w] = Class::Dead;
                continue;
            }
            let mut edge = graph.blocks[block].incoming.head;
            while edge != NONE {
                let link = graph.links[edge];
                let v = self.numbers.get(link.value);
                if v != UNVISITED {
                    if self.ancestor(w, v) {
                        self.back[w].append(&mut self.links, v);
                    } else {
                        self.incoming[w].add_unique(&mut self.links, v);
                    }
                }
                edge = link.next;
            }
        }
    }

    fn process_header(&mut self, forest: &mut Forest, w: usize, block: usize) -> bool {
        let mut pool = Vec::new();
        let mut edge = self.back[w].head;
        while edge != NONE {
            let link = self.links[edge];
            if link.value == w {
                self.class[w] = Class::SelfLoop;
            } else {
                pool.push(self.find_set(link.value));
            }
            edge = link.next;
        }
        let mut work = pool.clone();
        if !pool.is_empty() {
            self.class[w] = Class::Reducible;
        }
        let mut cursor = 0;
        while cursor < work.len() {
            let x = self.nodes[work[cursor]].ordinal;
            if self.incoming[x].count > MAX_NON_BACK_PREDS {
                return false;
            }
            let mut pred = self.incoming[x].head;
            while pred != NONE {
                let root = self.find_set(self.links[pred].value);
                let y = self.nodes[root].ordinal;
                if !self.ancestor(w, y) {
                    self.class[w] = Class::Irreducible;
                    self.incoming[w].add_unique(&mut self.links, y);
                } else if y != w && !pool.contains(&root) {
                    work.push(root);
                    pool.push(root);
                }
                pred = self.links[pred].next;
            }
            cursor += 1;
        }
        if !pool.is_empty() || self.class[w] == Class::SelfLoop {
            let index = forest.new_loop(block, self.class[w] != Class::Irreducible);
            self.nodes[w].loop_id = index;
            for node in pool {
                self.header[self.nodes[node].ordinal] = w;
                self.nodes[node].parent = w;
                let child = self.nodes[node].loop_id;
                if child == NONE {
                    forest.loops[index]
                        .blocks
                        .add_unique(&mut forest.links, self.nodes[node].block);
                } else {
                    forest.set_parent(child, index);
                }
            }
        }
        true
    }
}

fn find_loops(graph: &Graph, forest: &mut Forest) {
    let size = graph.blocks.len();
    let mut finder = Finder::new(size);
    for (index, block) in graph.blocks.iter().enumerate() {
        if block.present {
            finder.numbers.put(index, UNVISITED);
        }
    }
    finder.dfs(graph);
    finder.identify_edges(graph);
    finder.header[0] = 0;
    for w in (0..size).rev() {
        let block = finder.nodes[w].block;
        if block != NONE && !finder.process_header(forest, w, block) {
            return;
        }
    }
}

pub fn benchmark(size: i32) -> bool {
    let expected = match size {
        1 => 1605,
        15 => 1647,
        150 => 2052,
        1500 => 6102,
        15000 => 46602,
        _ => return false,
    };
    let mut graph = Graph::default();
    let mut forest = Forest::default();
    forest.new_loop(NONE, true);
    graph.create_node(0);
    graph.base_loop(0);
    graph.create_node(1);
    graph.add_edge(0, 2);
    for _ in 0..size {
        find_loops(&graph, &mut forest);
    }
    graph.build_cfg();
    find_loops(&graph, &mut forest);
    let mut extra = 0;
    for _ in 0..50 {
        let mut other = Forest::default();
        other.new_loop(NONE, true);
        find_loops(&graph, &mut other);
        extra += other.loops.len();
    }
    forest.nest();
    extra > 0 && forest.loops.len() == expected && graph.blocks.len() == 5213
}
