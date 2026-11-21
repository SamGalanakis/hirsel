// AWFY JavaScript list.js. The three lists share the lifetime of their pool.
namespace lists;

struct Element { value: i64, next: Element&<u32>? }

recursive fn make_list(pool: Element[>..]&, size: i64) -> Element&? {
    if size == 0 { return null; }
    let node .= pool.push(Element { value: size });
    node.next .= make_list(pool, size - 1);
    return node;
}

recursive fn length(node: Element&) -> i64 {
    let rest .= node.next;
    return if rest { 1 + length(rest) } else { 1 };
}

fn shorter(x: Element&?, y: Element&?) -> bool {
    var left .= x;
    var right .= y;
    while right {
        if !left { return true; }
        left .= left.next;
        right .= right.next;
    }
    return false;
}

recursive fn tail(x: Element&?, y: Element&?, z: Element&?) -> Element&? {
    if shorter(y, x) {
        if x && y && z {
            return tail(tail(x.next, y, z), tail(y.next, z, x), tail(z.next, x, y));
        }
        return null;
    }
    return z;
}

fn list_benchmark(first: i64) -> bool {
    var pool: Element[>..] = [];
    let x .= make_list(pool, first);
    let y .= make_list(pool, 10);
    let z .= make_list(pool, 6);
    let result .= tail(x, y, z);
    return result && length(result) == 10;
}
