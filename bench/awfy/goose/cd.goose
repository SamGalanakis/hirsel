// AWFY JavaScript cd.js. Red-black trees hold aircraft state and voxels.
// As in LangArena, the linked bucket contents live in one pool.
namespace cd;
import math;
import red_black;

const MIN = 0.0;
const MAX_XY = 1000.0;
const MAX_Z = 10.0;
const RADIUS = 1.0;
const VOXEL = RADIUS * 2.0;

struct V2 { x: f64, y: f64 }
struct V3 { x: f64, y: f64, z: f64 }
struct Aircraft { callsign: i64, position: V3 }
struct Motion { callsign: i64, from: V3, to: V3 }
struct Collision { first: i64, second: i64, position: V3 }

struct Bucket { head: i64 = -1, tail: i64 = -1, count: i64 = 0 }
struct BucketEntry { motion: Motion, next: i64 = -1 }
root struct Motions { buckets: Bucket[>..] = [], entries: BucketEntry[>..] = [] }

fn compare_numbers(a: f64, b: f64) -> red_black::Order {
    if a == b { return red_black::Order.Equal; }
    if a < b { return red_black::Order.Less; }
    if a > b { return red_black::Order.Greater; }
    return if a == a { red_black::Order.Greater } else { red_black::Order.Less };
}

fn compare_voxels(a: V2, b: V2) -> red_black::Order {
    let order = compare_numbers(a.x, b.x);
    return if order == red_black::Order.Equal { compare_numbers(a.y, b.y) } else { order };
}

fn compare_callsigns(a: i64, b: i64) -> red_black::Order {
    return if a < b { red_black::Order.Less }
        else if a == b { red_black::Order.Equal } else { red_black::Order.Greater };
}

fn dot(a: V3, b: V3) -> f64 {
    return a.x * b.x + a.y * b.y + a.z * b.z;
}

fn squared(a: V3) -> f64 { return dot(a, a); }

fn unit_interval(lo: f64, hi: f64) -> bool {
    return (lo <= 1.0 && 1.0 <= hi) || (lo <= 0.0 && 0.0 <= hi) ||
        (0.0 <= lo && hi <= 1.0);
}

fn intersection(m: Motion, other: Motion) -> V3, bool {
    let delta1 = m.to - m.from;
    let delta2 = other.to - other.from;
    let a = squared(delta2 - delta1);
    let no_point = V3 { x: 0.0, y: 0.0, z: 0.0 };
    if a == 0.0 {
        let distance = sqrt(squared(other.from - m.from));
        if distance <= RADIUS { return (m.from + other.from) * 0.5, true; }
        return no_point, false;
    }
    let b = 2.0 * dot(m.from - other.from, delta1 - delta2);
    let c = -RADIUS * RADIUS + squared(other.from - m.from);
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 { return no_point, false; }
    let lo = (-b - sqrt(discriminant)) / (2.0 * a);
    let hi = (-b + sqrt(discriminant)) / (2.0 * a);
    if lo <= hi && unit_interval(lo, hi) {
        let t = if lo <= 0.0 { 0.0 } else { lo };
        let position = ((m.from + delta1 * t) + (other.from + delta2 * t)) * 0.5;
        if MIN <= position.x && position.x <= MAX_XY &&
            MIN <= position.y && position.y <= MAX_XY &&
            MIN <= position.z && position.z <= MAX_Z {
            return position, true;
        }
    }
    return no_point, false;
}

fn crossing(edge: f64, coordinate: f64, velocity: f64) -> f64, f64 {
    let margin = RADIUS / 2.0;
    let lo = (edge - margin - coordinate) / velocity;
    let hi = (edge + VOXEL + margin - coordinate) / velocity;
    if velocity < 0.0 { return hi, lo; }
    return lo, hi;
}

fn in_voxel(voxel: V2, m: Motion) -> bool {
    if voxel.x < MIN || MAX_XY < voxel.x || voxel.y < MIN || MAX_XY < voxel.y {
        return false;
    }
    let dx = m.to.x - m.from.x;
    let dy = m.to.y - m.from.y;
    let margin = RADIUS / 2.0;
    let lx, hx = crossing(voxel.x, m.from.x, dx);
    let ly, hy = crossing(voxel.y, m.from.y, dy);
    return ((dx == 0.0 && voxel.x <= m.from.x + margin && m.from.x - margin <= voxel.x + VOXEL) ||
        unit_interval(lx, hx)) &&
        ((dy == 0.0 && voxel.y <= m.from.y + margin && m.from.y - margin <= voxel.y + VOXEL) ||
        unit_interval(ly, hy)) &&
        (dx == 0.0 || dy == 0.0 || (ly <= hx && hx <= hy) || (ly <= lx && lx <= hy) || (lx <= ly && hy <= hx));
}

fn voxel_hash(p: V3) -> V2 {
    let x = (p.x / VOXEL) as! i64;
    let y = (p.y / VOXEL) as! i64;
    return V2 { x: x * VOXEL - (if p.x < 0.0 { VOXEL } else { 0.0 }),
                y: y * VOXEL - (if p.y < 0.0 { VOXEL } else { 0.0 }) };
}

fn put_motion(tree: red_black::Tree<V2, i64>&, storage: Motions&, voxel: V2, motion: Motion) {
    let node = red_black::find(tree, voxel, compare_voxels);
    var bucket = 0;
    if node == 0 {
        bucket = storage.buckets.len;
        storage.buckets.push(Bucket {});
        red_black::put(tree, voxel, bucket, compare_voxels);
    } else {
        bucket = tree.entries[node - 1].value;
    }
    let index = storage.entries.len;
    storage.entries.push(BucketEntry { motion: motion });
    let group .= storage.buckets[bucket];
    if group.head < 0 { group.head = index; }
    else { storage.entries[group.tail].next = index; }
    group.tail = index;
    group.count += 1;
}

recursive fn recurse(tree: red_black::Tree<V2, i64>&, storage: Motions&,
                     seen: red_black::Tree<V2, bool>&, voxel: V2, motion: Motion) {
    if !in_voxel(voxel, motion) { return; }
    let _, present = red_black::put(seen, voxel, true, compare_voxels);
    if present { return; }
    put_motion(tree, storage, voxel, motion);
    let x = voxel.x;
    let y = voxel.y;
    recurse(tree, storage, seen, V2 { x: x - VOXEL, y: y }, motion);
    recurse(tree, storage, seen, V2 { x: x + VOXEL, y: y }, motion);
    recurse(tree, storage, seen, V2 { x: x, y: y - VOXEL }, motion);
    recurse(tree, storage, seen, V2 { x: x, y: y + VOXEL }, motion);
    recurse(tree, storage, seen, V2 { x: x - VOXEL, y: y - VOXEL }, motion);
    recurse(tree, storage, seen, V2 { x: x - VOXEL, y: y + VOXEL }, motion);
    recurse(tree, storage, seen, V2 { x: x + VOXEL, y: y - VOXEL }, motion);
    recurse(tree, storage, seen, V2 { x: x + VOXEL, y: y + VOXEL }, motion);
}

fn collisions(motions: const Motion[:]) -> i64 {
    var tree = red_black::Tree<V2, i64> { .. };
    red_black::initialize(tree);
    var storage = Motions { .. };
    for motion in motions {
        var seen = red_black::Tree<V2, bool> { .. };
        red_black::initialize(seen);
        recurse(tree, storage, seen, voxel_hash(motion.from), motion);
    }
    var reduced: i64[>..] = [];
    var node = red_black::first(tree);
    while node != 0 {
        let bucket = tree.entries[node - 1].value;
        if storage.buckets[bucket].count > 1 { reduced.push(bucket); }
        node = red_black::next(tree, node);
    }
    var result: Collision[>..] = [];
    for bucket in reduced {
        var i = storage.buckets[bucket].head;
        while i >= 0 {
            let first = storage.entries[i].motion;
            var j = storage.entries[i].next;
            while j >= 0 {
                let second = storage.entries[j].motion;
                let point, found = intersection(first, second);
                if found { result.push(Collision { first: first.callsign, second: second.callsign, position: point }); }
                j = storage.entries[j].next;
            }
            i = storage.entries[i].next;
        }
    }
    return result.len;
}

fn handle_frame(state: red_black::Tree<i64, V3>&, frame: const Aircraft[:]) -> i64 {
    var seen = red_black::Tree<i64, bool> { .. };
    red_black::initialize(seen);
    var motions: Motion[>..] = [];
    for aircraft in frame {
        let current = aircraft.position;
        let old, _ = red_black::put(state, aircraft.callsign, current, compare_callsigns);
        red_black::put(seen, aircraft.callsign, true, compare_callsigns);
        motions.push(Motion { callsign: aircraft.callsign, from: old, to: current });
    }
    var removed: i64[>..] = [];
    var node = red_black::first(state);
    while node != 0 {
        let callsign = state.entries[node - 1].key;
        if red_black::find(seen, callsign, compare_callsigns) == 0 { removed.push(callsign); }
        node = red_black::next(state, node);
    }
    for callsign in removed { red_black::remove(state, callsign, compare_callsigns); }
    return collisions(motions);
}

fn simulate(size: i64, time: f64, frame: Aircraft[>..]&) {
    var i = 0;
    while i < size {
        let lane = i * 3;
        frame.push(Aircraft { callsign: i, position: V3 { x: time, y: cos(time) * 2.0 + lane, z: 10.0 } });
        frame.push(Aircraft { callsign: i + 1, position: V3 { x: time, y: sin(time) * 2.0 + lane, z: 10.0 } });
        i += 2;
    }
}

fn benchmark(size: i64) -> bool {
    let expected = if size == 2 { 42 } else if size == 10 { 390 } else if size == 100 { 4305 }
        else if size == 200 { 8655 } else if size == 250 { 10830 }
        else if size == 500 || size == 1000 { 14484 } else { -1 };
    if expected < 0 { return false; }
    var state = red_black::Tree<i64, V3> { .. };
    red_black::initialize(state);
    var total = 0;
    for i in 200 {
        var frame: Aircraft[>..] = [];
        simulate(size, i / 10.0, frame);
        total += handle_frame(state, frame);
    }
    return total == expected;
}
