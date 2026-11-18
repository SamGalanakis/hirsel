// AWFY list.js. The three lists share one grow-only pool of explicit links.
namespace list_index;

struct Element { value: i64, next: i64 = -1 }

recursive fn make_list(pool: Element[>..]&, size: i64) -> i64 {
    if size == 0 { return -1; }
    let node = pool.len;
    pool.push(Element { value: size });
    let rest = make_list(pool, size - 1);
    pool[node].next = rest;
    return node;
}

fn shorter(pool: const Element[:], x: i64, y: i64) -> bool {
    var left = x;
    var right = y;
    while right >= 0 {
        if left < 0 { return true; }
        left = pool[left].next;
        right = pool[right].next;
    }
    return false;
}

recursive fn tail(pool: const Element[:], x: i64, y: i64, z: i64) -> i64 {
    if !shorter(pool, y, x) { return z; }
    if x < 0 || y < 0 || z < 0 { return -1; }
    return tail(pool, tail(pool, pool[x].next, y, z), tail(pool, pool[y].next, z, x), tail(pool, pool[z].next, x, y));
}

recursive fn length(pool: const Element[:], node: i64) -> i64 {
    return if node < 0 { 0 } else { 1 + length(pool, pool[node].next) };
}

fn list_benchmark() -> bool {
    var pool: Element[>..] = [];
    let x = make_list(pool, 15);
    let y = make_list(pool, 10);
    let z = make_list(pool, 6);
    return length(pool, tail(pool, x, y, z)) == 10;
}
