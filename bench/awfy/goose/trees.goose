// AWFY JavaScript storage.js and towers.js.
namespace trees;
import small;

struct Tree { start: i64, count: i64 }

root struct TreePool {
    arrays: Tree[>..] = [],
    slots: i64[>..] = []
}

recursive fn build_tree(pool: TreePool&, depth: i64, random: small::Random&, count: i64&) -> i64 {
    count += 1;
    let length = if depth == 1 { random.next() % 10 + 1 } else { 4 };
    let index = pool.arrays.len;
    let start = pool.slots.len;
    pool.arrays.push(Tree { start: start, count: length });
    pool.slots.resize(start + length, -1);
    if depth > 1 {
        for i in length {
            let child = build_tree(pool, depth - 1, random, count);
            pool.slots[start + i] = child;
        }
    }
    return index;
}

fn storage(depth: i64) -> bool {
    var pool = TreePool { .. };
    var random = small::Random {};
    var count = 0;
    build_tree(pool, depth, random, count);
    return count == 5461;
}

struct Disk { size: i64, next: i64 }
struct Towers { piles: i64[3], moves: i64 = 0 }
// Each pile is an index into the disk pool. The list connections stay in that pool.
fn push(state: Towers&, pool: Disk[>..]&, disk: i64, pile: i64) {
    let old = state.piles[pile];
    if old >= 0 && pool[disk].size >= pool[old].size {
        abort("disk is too large");
    }
    pool[disk].next = old;
    state.piles[pile] = disk;
}

fn move_top(state: Towers&, pool: Disk[>..]&, from: i64, to: i64) {
    let disk = state.piles[from];
    if disk < 0 { abort("empty pile"); }
    state.piles[from] = pool[disk].next;
    pool[disk].next = -1;
    state.push(pool, disk, to);
    state.moves += 1;
}

recursive fn move_disks(state: Towers&, pool: Disk[>..]&, count: i64, from: i64, to: i64) {
    if count == 1 {
        state.move_top(pool, from, to);
    } else {
        let other = 3 - from - to;
        state.move_disks(pool, count - 1, from, other);
        state.move_top(pool, from, to);
        state.move_disks(pool, count - 1, other, to);
    }
}

fn towers(count: i64) -> bool {
    var state = Towers { piles: [-1, -1, -1] };
    var pool: Disk[>..] = [];
    var size = count + 1;
    while size > 0 {
        size -= 1;
        pool.push(Disk { size: size, next: -1 });
        state.push(pool, pool.len - 1, 0);
    }
    state.move_disks(pool, count, 0, 1);
    return state.moves == 8191;
}
