import cli;

struct Node { left: i64, right: i64 }
struct Count { nodes: i64 }

recursive fn tree(nodes: Node[>..]&, depth: i64) -> i64 {
    var left = -1;
    var right = -1;
    if depth > 0 {
        left = tree(nodes, depth - 1);
        right = tree(nodes, depth - 1);
    }
    let index = nodes.len;
    nodes.push(Node { left, right });
    return index;
}

recursive fn count(nodes: Node[:], index: i64) -> i64 {
    let node = nodes[index];
    if node.left < 0 { return 1; }
    return 1 + count(nodes, node.left) + count(nodes, node.right);
}

fn build_count(depth: i64) -> i64 {
    var nodes: Node[>..] = [];
    let root = tree(nodes, depth);
    return count(nodes, root);
}

thread_fn batch(depth: i64, iterations: i64) {
    var total = 0;
    for i in iterations { total += build_count(depth); }
    qput(Count { total });
}

fn main() {
    let depth = max(number(1), 6);
    let workers = number(2);
    let stretch = depth + 1;
    print("stretch tree of depth ", stretch, "\t check: ", build_count(stretch));
    var long_lived: Node[>..] = [];
    let root = tree(long_lived, depth);
    for half in 2..depth / 2 + 1 {
        let d = half * 2;
        let iterations = 1 << (depth - d + 4);
        var ids: i64[>..] = [];
        for i in workers {
            let jobs = iterations * (i + 1) / workers - iterations * i / workers;
            ids.push(thread_spawn(batch, d, jobs));
        }
        var total = 0;
        for i in workers { total += qget<Count>().nodes; }
        for id in ids { thread_wait(id); }
        print(iterations, "\t trees of depth ", d, "\t check: ", total);
    }
    print("long lived tree of depth ", depth, "\t check: ", count(long_lived, root));
}
