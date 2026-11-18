// AWFY JavaScript havlak.js and som.js. Graph, loop and predecessor lists
// are append-only pools, with independent chains preserving insertion order.
namespace havlak_pool;
import std;

const UNVISITED = 2147483647;
const MAX_NON_BACK_PREDS = 32768;

struct Chain { head: i64 = -1, tail: i64 = -1, count: i64 = 0 }
struct Link { value: i64, next: i64 = -1 }

fn append(list: Chain&, pool: Link[>..]&, value: i64) {
    let index = pool.len;
    pool.push(Link { value: value });
    if list.head < 0 { list.head = index; }
    else { pool[list.tail].next = index; }
    list.tail = index;
    list.count += 1;
}

fn contains(list: Chain&, pool: const Link[:], value: i64) -> bool {
    var index = list.head;
    while index >= 0 {
        if pool[index].value == value { return true; }
        index = pool[index].next;
    }
    return false;
}

fn add_unique(list: Chain&, pool: Link[>..]&, value: i64) {
    if !contains(list, pool, value) { append(list, pool, value); }
}

struct Block { incoming: Chain = Chain {}, outgoing: Chain = Chain {}, present: bool = false }
struct Edge { from: i64, to: i64 }

root struct Graph {
    blocks: Block[>..] = [],
    edges: Edge[>..] = [],
    links: Link[>..] = []
}

fn create_node(graph: Graph&, name: i64) {
    while graph.blocks.len <= name { graph.blocks.push(Block {}); }
    graph.blocks[name].present = true;
}

fn add_edge(graph: Graph&, from: i64, to: i64) {
    create_node(graph, from);
    create_node(graph, to);
    append(graph.blocks[from].outgoing, graph.links, to);
    append(graph.blocks[to].incoming, graph.links, from);
    graph.edges.push(Edge { from: from, to: to });
}

fn straight(graph: Graph&, from: i64, count: i64) -> i64 {
    for i in count { add_edge(graph, from + i, from + i + 1); }
    return from + count;
}

fn diamond(graph: Graph&, start: i64) -> i64 {
    add_edge(graph, start, start + 1);
    add_edge(graph, start, start + 2);
    add_edge(graph, start + 1, start + 3);
    add_edge(graph, start + 2, start + 3);
    return start + 3;
}

fn base_loop(graph: Graph&, from: i64) -> i64 {
    let header = straight(graph, from, 1);
    let first = diamond(graph, header);
    let second_start = straight(graph, first, 1);
    let second = diamond(graph, second_start);
    let footer = straight(graph, second, 1);
    add_edge(graph, second, second_start);
    add_edge(graph, first, header);
    add_edge(graph, footer, from);
    return straight(graph, footer, 1);
}

fn build_cfg(graph: Graph&) {
    var n = 2;
    for _ in 10 {
        create_node(graph, n + 1);
        add_edge(graph, 2, n + 1);
        n += 1;
        for _ in 10 {
            let top = n;
            n = straight(graph, n, 1);
            for _ in 5 { n = base_loop(graph, n); }
            let bottom = straight(graph, n, 1);
            add_edge(graph, n, top);
            n = bottom;
        }
        add_edge(graph, n, 1);
    }
}

struct SimpleLoop {
    blocks: Chain = Chain {},
    children: Chain = Chain {},
    parent: i64 = -1,
    root: bool,
    reducible: bool,
    depth: i64 = 0,
    nesting: i64 = 0
}

root struct Forest { loops: SimpleLoop[>..] = [], links: Link[>..] = [] }

fn new_loop(forest: Forest&, block_id: i64, reducible: bool) -> i64 {
    let index = forest.loops.len;
    forest.loops.push(SimpleLoop { root: block_id < 0, reducible: reducible });
    if block_id >= 0 { append(forest.loops[index].blocks, forest.links, block_id); }
    return index;
}

fn set_parent(forest: Forest&, child: i64, parent: i64) {
    forest.loops[child].parent = parent;
    add_unique(forest.loops[parent].children, forest.links, child);
}

recursive fn nesting(forest: Forest&, index: i64, depth: i64) {
    forest.loops[index].depth = depth;
    var edge = forest.loops[index].children.head;
    while edge >= 0 {
        let child = forest.links[edge].value;
        nesting(forest, child, depth + 1);
        let level = forest.loops[child].nesting + 1;
        if level > forest.loops[index].nesting { forest.loops[index].nesting = level; }
        edge = forest.links[edge].next;
    }
}

fn nest_forest(forest: Forest&) {
    for i in forest.loops.len {
        if !forest.loops[i].root && forest.loops[i].parent < 0 { set_parent(forest, i, 0); }
    }
    nesting(forest, 0, 0);
}

enum Kind { Top, NonHeader, Reducible, SelfLoop, Irreducible, Dead }
struct UnionNode { parent: i64 = 0, block_id: i64 = -1, loop_id: i64 = -1, ordinal: i64 = 0 }
struct Number { key: i64, hash: i64, value: i64, next: i64 = -1 }

root struct Finder {
    incoming: Chain[>..] = [],
    back: Chain[>..] = [],
    links: Link[>..] = [],
    buckets: i64[>..<] = [],
    numbers: Number[>..] = [],
    header: i64[>..] = [],
    kind: Kind[>..] = [],
    last: i64[>..] = [],
    nodes: UnionNode[>..] = []
}

fn get(f: Finder&, key: i64) -> i64 {
    let hash = key ^ (key >> 16);
    var index = f.buckets[hash & (f.buckets.len - 1)];
    while index >= 0 {
        let entry .= f.numbers[index];
        if entry.key == key && entry.hash == hash { return entry.value; }
        index = entry.next;
    }
    return -1;
}

fn transfer(f: Finder&) {
    let size = f.buckets.len;
    f.buckets.resize(size * 2, -1);
    for i in size {
        var index = f.buckets[i];
        f.buckets[i] = -1;
        var low_tail = -1;
        var high_tail = -1;
        while index >= 0 {
            let entry = f.numbers[index];
            if (entry.hash & size) == 0 {
                if low_tail >= 0 { f.numbers[low_tail].next = index; }
                else { f.buckets[i] = index; }
                low_tail = index;
            } else {
                if high_tail >= 0 { f.numbers[high_tail].next = index; }
                else { f.buckets[i + size] = index; }
                high_tail = index;
            }
            index = entry.next;
        }
        if low_tail >= 0 { f.numbers[low_tail].next = -1; }
        if high_tail >= 0 { f.numbers[high_tail].next = -1; }
    }
}

fn put(f: Finder&, key: i64, value: i64) {
    let hash = key ^ (key >> 16);
    let slot = hash & (f.buckets.len - 1);
    var index = f.buckets[slot];
    var prev = -1;
    while index >= 0 {
        let entry .= f.numbers[index];
        if entry.key == key && entry.hash == hash {
            entry.value = value;
            return;
        }
        prev = index;
        index = entry.next;
    }
    let added = f.numbers.len;
    f.numbers.push(Number { key: key, hash: hash, value: value });
    if prev >= 0 { f.numbers[prev].next = added; }
    else { f.buckets[slot] = added; }
    if f.numbers.len > f.buckets.len { transfer(f); }
}

struct Frame { ordinal: i64, edge: i64 }

fn enter(graph: Graph&, f: Finder&, path: Frame[>..<]&, block_id: i64, ordinal: i64) {
    f.nodes[ordinal] = UnionNode { parent: ordinal, block_id: block_id, ordinal: ordinal };
    put(f, block_id, ordinal);
    path.push(Frame { ordinal: ordinal, edge: graph.blocks[block_id].outgoing.head });
}

fn dfs(graph: Graph&, f: Finder&) {
    var path: Frame[>..<] = [];
    var last = 0;
    enter(graph, f, path, 0, last);
    while path.len != 0 {
        let top = path.len - 1;
        let frame = path[top];
        if frame.edge < 0 {
            f.last[frame.ordinal] = last;
            path.resize(top);
        } else {
            let target = graph.links[frame.edge].value;
            path[top].edge = graph.links[frame.edge].next;
            if get(f, target) == UNVISITED {
                last += 1;
                enter(graph, f, path, target, last);
            }
        }
    }
}

fn ancestor(f: Finder&, w: i64, v: i64) -> bool {
    return w <= v && v <= f.last[w];
}

fn find_set(f: Finder&, start: i64) -> i64 {
    var path: i64[>..] = [];
    var node = start;
    while node != f.nodes[node].parent {
        let parent = f.nodes[node].parent;
        if parent != f.nodes[parent].parent { path.push(node); }
        node = parent;
    }
    let parent = f.nodes[start].parent;
    for i in path { f.nodes[i].parent = parent; }
    return node;
}

fn identify_edges(graph: Graph&, f: Finder&, size: i64) {
    for w in size {
        f.header[w] = 0;
        f.kind[w] = Kind.NonHeader;
        let block_id = f.nodes[w].block_id;
        if block_id >= 0 {
            var edge = graph.blocks[block_id].incoming.head;
            while edge >= 0 {
                let v = get(f, graph.links[edge].value);
                if v != UNVISITED {
                    if ancestor(f, w, v) { append(f.back[w], f.links, v); }
                    else { add_unique(f.incoming[w], f.links, v); }
                }
                edge = graph.links[edge].next;
            }
        } else {
            f.kind[w] = Kind.Dead;
        }
    }
}

fn seen(pool: const i64[:], target: i64) -> bool {
    for x in pool { if x == target { return true; } }
    return false;
}

fn process_header(f: Finder&, forest: Forest&, w: i64, block_id: i64) -> bool {
    var pool: i64[>..] = [];
    var edge = f.back[w].head;
    while edge >= 0 {
        let v = f.links[edge].value;
        if v == w { f.kind[w] = Kind.SelfLoop; }
        else { pool.push(find_set(f, v)); }
        edge = f.links[edge].next;
    }
    var work: i64[>..] = [];
    work.append(pool);
    if pool.len != 0 { f.kind[w] = Kind.Reducible; }

    var cursor = 0;
    while cursor < work.len {
        let x = f.nodes[work[cursor]].ordinal;
        if f.incoming[x].count > MAX_NON_BACK_PREDS { return false; }
        var pred = f.incoming[x].head;
        while pred >= 0 {
            let root = find_set(f, f.links[pred].value);
            let y = f.nodes[root].ordinal;
            if !ancestor(f, w, y) {
                f.kind[w] = Kind.Irreducible;
                add_unique(f.incoming[w], f.links, y);
            } else if y != w && !seen(pool, root) {
                work.push(root);
                pool.push(root);
            }
            pred = f.links[pred].next;
        }
        cursor += 1;
    }

    if pool.len > 0 || f.kind[w] == Kind.SelfLoop {
        let index = new_loop(forest, block_id, f.kind[w] != Kind.Irreducible);
        f.nodes[w].loop_id = index;
        for node in pool {
            f.header[f.nodes[node].ordinal] = w;
            f.nodes[node].parent = w;
            let child = f.nodes[node].loop_id;
            if child >= 0 { set_parent(forest, child, index); }
            else { add_unique(forest.loops[index].blocks, forest.links, f.nodes[node].block_id); }
        }
    }
    return true;
}

fn find_loops(graph: Graph&, forest: Forest&) {
    let size = graph.blocks.len;
    var f = Finder { .. };
    f.buckets.resize(16, -1);
    for _ in size {
        f.incoming.push(Chain {});
        f.back.push(Chain {});
        f.header.push(0);
        f.kind.push(Kind.Top);
        f.last.push(0);
        f.nodes.push(UnionNode {});
    }
    for i in size { if graph.blocks[i].present { put(f, i, UNVISITED); } }
    dfs(graph, f);
    identify_edges(graph, f, size);
    f.header[0] = 0;
    var w = size;
    while w > 0 {
        w -= 1;
        let block_id = f.nodes[w].block_id;
        if block_id >= 0 && !process_header(f, forest, w, block_id) { return; }
    }
}

fn benchmark(size: i64) -> bool {
    let expected = if size == 15000 { 46602 } else if size == 1500 { 6102 } else
        if size == 150 { 2052 } else if size == 15 { 1647 } else if size == 1 { 1605 } else { -1 };
    if expected < 0 { return false; }
    var graph = Graph { .. };
    var forest = Forest { .. };
    new_loop(forest, -1, true);
    create_node(graph, 0);
    base_loop(graph, 0);
    create_node(graph, 1);
    add_edge(graph, 0, 2);
    for _ in size { find_loops(graph, forest); }
    build_cfg(graph);
    find_loops(graph, forest);
    var extra = 0;
    for _ in 50 {
        var other = Forest { .. };
        new_loop(other, -1, true);
        find_loops(graph, other);
        extra += other.loops.len;
    }
    nest_forest(forest);
    return extra > 0 && forest.loops.len == expected && graph.blocks.len == 5213;
}
