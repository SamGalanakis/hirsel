// AWFY CD, after the Java reference: collision detection over red-black
// trees. The generic tree keeps its nodes in an arena with index links and
// parent pointers, as the reference's nodes do.
use std::cmp::Ordering;

const MIN_X: f64 = 0.0;
const MIN_Y: f64 = 0.0;
const MAX_X: f64 = 1000.0;
const MAX_Y: f64 = 1000.0;
const MIN_Z: f64 = 0.0;
const MAX_Z: f64 = 10.0;
const PROXIMITY_RADIUS: f64 = 1.0;
const GOOD_VOXEL_SIZE: f64 = PROXIMITY_RADIUS * 2.0;

#[derive(Clone, Copy)]
struct V3 {
    x: f64,
    y: f64,
    z: f64,
}
impl V3 {
    fn plus(self, o: V3) -> V3 {
        V3 {
            x: self.x + o.x,
            y: self.y + o.y,
            z: self.z + o.z,
        }
    }
    fn minus(self, o: V3) -> V3 {
        V3 {
            x: self.x - o.x,
            y: self.y - o.y,
            z: self.z - o.z,
        }
    }
    fn dot(self, o: V3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn squared_magnitude(self) -> f64 {
        self.dot(self)
    }
    fn magnitude(self) -> f64 {
        self.squared_magnitude().sqrt()
    }
    fn times(self, a: f64) -> V3 {
        V3 {
            x: self.x * a,
            y: self.y * a,
            z: self.z * a,
        }
    }
}

#[derive(Clone, Copy)]
struct V2 {
    x: f64,
    y: f64,
}
impl V2 {
    fn plus(self, o: V2) -> V2 {
        V2 {
            x: self.x + o.x,
            y: self.y + o.y,
        }
    }
    fn minus(self, o: V2) -> V2 {
        V2 {
            x: self.x - o.x,
            y: self.y - o.y,
        }
    }
}

fn compare_numbers(a: f64, b: f64) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    if a < b {
        return Ordering::Less;
    }
    if a > b {
        return Ordering::Greater;
    }
    if a == a {
        return Ordering::Greater;
    }
    Ordering::Less
}

trait Key: Copy {
    fn cmp_key(&self, o: &Self) -> Ordering;
}
impl Key for V2 {
    fn cmp_key(&self, o: &V2) -> Ordering {
        match compare_numbers(self.x, o.x) {
            Ordering::Equal => compare_numbers(self.y, o.y),
            r => r,
        }
    }
}
impl Key for i32 {
    fn cmp_key(&self, o: &i32) -> Ordering {
        self.cmp(o)
    }
}

const NIL: usize = 0;

struct Links {
    left: usize,
    right: usize,
    parent: usize,
    red: bool,
}

struct TreeEntry<K, V> {
    key: K,
    value: V,
}

struct Rbt<K, V> {
    links: Vec<Links>,
    entries: Vec<TreeEntry<K, V>>,
    root: usize,
}

impl<K: Key, V: Clone> Rbt<K, V> {
    fn new() -> Self {
        Rbt {
            links: vec![Links {
                left: NIL,
                right: NIL,
                parent: NIL,
                red: false,
            }],
            entries: Vec::new(),
            root: NIL,
        }
    }
    fn is_red(&self, i: usize) -> bool {
        i != NIL && self.links[i].red
    }
    fn tree_minimum(&self, x: usize) -> usize {
        let mut c = x;
        while self.links[c].left != NIL {
            c = self.links[c].left;
        }
        c
    }
    fn successor(&self, x0: usize) -> usize {
        let mut x = x0;
        if self.links[x].right != NIL {
            return self.tree_minimum(self.links[x].right);
        }
        let mut y = self.links[x].parent;
        while y != NIL && x == self.links[y].right {
            x = y;
            y = self.links[y].parent;
        }
        y
    }
    fn find_node(&self, key: &K) -> usize {
        let mut c = self.root;
        while c != NIL {
            match key.cmp_key(&self.entries[c - 1].key) {
                Ordering::Equal => return c,
                Ordering::Less => c = self.links[c].left,
                Ordering::Greater => c = self.links[c].right,
            }
        }
        NIL
    }
    fn get(&self, key: &K) -> Option<V> {
        let n = self.find_node(key);
        if n == NIL {
            None
        } else {
            Some(self.entries[n - 1].value.clone())
        }
    }
    // treeInsert: (new entry, the new node, the old value)
    fn tree_insert(&mut self, key: K, value: V) -> (bool, usize, Option<V>) {
        let mut y = NIL;
        let mut x = self.root;
        while x != NIL {
            y = x;
            match key.cmp_key(&self.entries[x - 1].key) {
                Ordering::Less => x = self.links[x].left,
                Ordering::Greater => x = self.links[x].right,
                Ordering::Equal => {
                    let old = std::mem::replace(&mut self.entries[x - 1].value, value);
                    return (false, NIL, Some(old));
                }
            }
        }
        self.entries.push(TreeEntry { key, value });
        self.links.push(Links {
            left: NIL,
            right: NIL,
            parent: y,
            red: true,
        });
        let z = self.links.len() - 1;
        if y == NIL {
            self.root = z;
        } else if key.cmp_key(&self.entries[y - 1].key) == Ordering::Less {
            self.links[y].left = z;
        } else {
            self.links[y].right = z;
        }
        (true, z, None)
    }
    fn left_rotate(&mut self, x: usize) -> usize {
        let y = self.links[x].right;
        self.links[x].right = self.links[y].left;
        if self.links[y].left != NIL {
            let yl = self.links[y].left;
            self.links[yl].parent = x;
        }
        self.links[y].parent = self.links[x].parent;
        let xp = self.links[x].parent;
        if xp == NIL {
            self.root = y;
        } else if x == self.links[xp].left {
            self.links[xp].left = y;
        } else {
            self.links[xp].right = y;
        }
        self.links[y].left = x;
        self.links[x].parent = y;
        y
    }
    fn right_rotate(&mut self, y: usize) -> usize {
        let x = self.links[y].left;
        self.links[y].left = self.links[x].right;
        if self.links[x].right != NIL {
            let xr = self.links[x].right;
            self.links[xr].parent = y;
        }
        self.links[x].parent = self.links[y].parent;
        let yp = self.links[y].parent;
        if yp == NIL {
            self.root = x;
        } else if y == self.links[yp].left {
            self.links[yp].left = x;
        } else {
            self.links[yp].right = x;
        }
        self.links[x].right = y;
        self.links[y].parent = x;
        x
    }
    fn put(&mut self, key: K, value: V) -> Option<V> {
        let (is_new, z, old) = self.tree_insert(key, value);
        if !is_new {
            return old;
        }
        let mut x = z;
        while x != self.root && self.is_red(self.links[x].parent) {
            let xp = self.links[x].parent;
            let xpp = self.links[xp].parent;
            if xp == self.links[xpp].left {
                let y = self.links[xpp].right;
                if self.is_red(y) {
                    self.links[xp].red = false;
                    self.links[y].red = false;
                    self.links[xpp].red = true;
                    x = xpp;
                } else {
                    if x == self.links[xp].right {
                        x = xp;
                        self.left_rotate(x);
                    }
                    let p = self.links[x].parent;
                    let pp = self.links[p].parent;
                    self.links[p].red = false;
                    self.links[pp].red = true;
                    self.right_rotate(pp);
                }
            } else {
                let y = self.links[xpp].left;
                if self.is_red(y) {
                    self.links[xp].red = false;
                    self.links[y].red = false;
                    self.links[xpp].red = true;
                    x = xpp;
                } else {
                    if x == self.links[xp].left {
                        x = xp;
                        self.right_rotate(x);
                    }
                    let p = self.links[x].parent;
                    let pp = self.links[p].parent;
                    self.links[p].red = false;
                    self.links[pp].red = true;
                    self.left_rotate(pp);
                }
            }
        }
        let r = self.root;
        self.links[r].red = false;
        None
    }
    fn remove(&mut self, key: &K) -> Option<V> {
        let z = self.find_node(key);
        if z == NIL {
            return None;
        }
        let y = if self.links[z].left == NIL || self.links[z].right == NIL {
            z
        } else {
            self.successor(z)
        };
        let x = if self.links[y].left != NIL {
            self.links[y].left
        } else {
            self.links[y].right
        };
        let x_parent;
        if x != NIL {
            self.links[x].parent = self.links[y].parent;
            x_parent = self.links[x].parent;
        } else {
            x_parent = self.links[y].parent;
        }
        let yp = self.links[y].parent;
        if yp == NIL {
            self.root = x;
        } else if y == self.links[yp].left {
            self.links[yp].left = x;
        } else {
            self.links[yp].right = x;
        }
        if y != z {
            if !self.links[y].red {
                self.remove_fixup(x, x_parent);
            }
            self.links[y].parent = self.links[z].parent;
            self.links[y].red = self.links[z].red;
            self.links[y].left = self.links[z].left;
            self.links[y].right = self.links[z].right;
            if self.links[z].left != NIL {
                let l = self.links[z].left;
                self.links[l].parent = y;
            }
            if self.links[z].right != NIL {
                let r = self.links[z].right;
                self.links[r].parent = y;
            }
            let zp = self.links[z].parent;
            if zp != NIL {
                if self.links[zp].left == z {
                    self.links[zp].left = y;
                } else {
                    self.links[zp].right = y;
                }
            } else {
                self.root = y;
            }
        } else if !self.links[y].red {
            self.remove_fixup(x, x_parent);
        }
        Some(self.entries[z - 1].value.clone())
    }
    fn remove_fixup(&mut self, mut x: usize, mut x_parent: usize) {
        while x != self.root && (x == NIL || !self.links[x].red) {
            if x == self.links[x_parent].left {
                let mut w = self.links[x_parent].right;
                if self.links[w].red {
                    self.links[w].red = false;
                    self.links[x_parent].red = true;
                    self.left_rotate(x_parent);
                    w = self.links[x_parent].right;
                }
                if !self.is_red(self.links[w].left) && !self.is_red(self.links[w].right) {
                    self.links[w].red = true;
                    x = x_parent;
                    x_parent = self.links[x].parent;
                } else {
                    if !self.is_red(self.links[w].right) {
                        let wl = self.links[w].left;
                        self.links[wl].red = false;
                        self.links[w].red = true;
                        self.right_rotate(w);
                        w = self.links[x_parent].right;
                    }
                    self.links[w].red = self.links[x_parent].red;
                    self.links[x_parent].red = false;
                    if self.links[w].right != NIL {
                        let wr = self.links[w].right;
                        self.links[wr].red = false;
                    }
                    self.left_rotate(x_parent);
                    x = self.root;
                    x_parent = self.links[x].parent;
                }
            } else {
                let mut w = self.links[x_parent].left;
                if self.links[w].red {
                    self.links[w].red = false;
                    self.links[x_parent].red = true;
                    self.right_rotate(x_parent);
                    w = self.links[x_parent].left;
                }
                if !self.is_red(self.links[w].right) && !self.is_red(self.links[w].left) {
                    self.links[w].red = true;
                    x = x_parent;
                    x_parent = self.links[x].parent;
                } else {
                    if !self.is_red(self.links[w].left) {
                        let wr = self.links[w].right;
                        self.links[wr].red = false;
                        self.links[w].red = true;
                        self.left_rotate(w);
                        w = self.links[x_parent].left;
                    }
                    self.links[w].red = self.links[x_parent].red;
                    self.links[x_parent].red = false;
                    if self.links[w].left != NIL {
                        let wl = self.links[w].left;
                        self.links[wl].red = false;
                    }
                    self.right_rotate(x_parent);
                    x = self.root;
                    x_parent = self.links[x].parent;
                }
            }
        }
        if x != NIL {
            self.links[x].red = false;
        }
    }
    // forEach over entries, in order.
    fn for_each<F: FnMut(&K, &V)>(&self, mut f: F) {
        if self.root == NIL {
            return;
        }
        let mut c = self.tree_minimum(self.root);
        while c != NIL {
            f(&self.entries[c - 1].key, &self.entries[c - 1].value);
            c = self.successor(c);
        }
    }
}

#[derive(Clone, Copy)]
struct Motion {
    callsign: i32,
    pos_one: V3,
    pos_two: V3,
}

impl Motion {
    fn delta(&self) -> V3 {
        self.pos_two.minus(self.pos_one)
    }
    fn find_intersection(&self, other: &Motion) -> Option<V3> {
        let init1 = self.pos_one;
        let init2 = other.pos_one;
        let vec1 = self.delta();
        let vec2 = other.delta();
        let radius = PROXIMITY_RADIUS;
        let a = vec2.minus(vec1).squared_magnitude();
        if a != 0.0 {
            let b = 2.0 * init1.minus(init2).dot(vec1.minus(vec2));
            let c = -radius * radius + init2.minus(init1).squared_magnitude();
            let discr = b * b - 4.0 * a * c;
            if discr < 0.0 {
                return None;
            }
            let v1 = (-b - discr.sqrt()) / (2.0 * a);
            let v2 = (-b + discr.sqrt()) / (2.0 * a);
            if v1 <= v2
                && ((v1 <= 1.0 && 1.0 <= v2)
                    || (v1 <= 0.0 && 0.0 <= v2)
                    || (0.0 <= v1 && v2 <= 1.0))
            {
                let v = if v1 <= 0.0 { 0.0 } else { v1 };
                let result1 = init1.plus(vec1.times(v));
                let result2 = init2.plus(vec2.times(v));
                let result = result1.plus(result2).times(0.5);
                if result.x >= MIN_X
                    && result.x <= MAX_X
                    && result.y >= MIN_Y
                    && result.y <= MAX_Y
                    && result.z >= MIN_Z
                    && result.z <= MAX_Z
                {
                    return Some(result);
                }
            }
            return None;
        }
        let dist = init2.minus(init1).magnitude();
        if dist <= radius {
            return Some(init1.plus(init2).times(0.5));
        }
        None
    }
}

struct Aircraft {
    callsign: i32,
    position: V3,
}
#[allow(dead_code)]
struct Collision {
    a: i32,
    b: i32,
    position: V3,
}

fn is_in_voxel(voxel: V2, motion: &Motion) -> bool {
    if voxel.x > MAX_X || voxel.x < MIN_X || voxel.y > MAX_Y || voxel.y < MIN_Y {
        return false;
    }
    let init = motion.pos_one;
    let fin = motion.pos_two;
    let v_s = GOOD_VOXEL_SIZE;
    let r = PROXIMITY_RADIUS / 2.0;
    let v_x = voxel.x;
    let x0 = init.x;
    let xv = fin.x - init.x;
    let v_y = voxel.y;
    let y0 = init.y;
    let yv = fin.y - init.y;
    let mut low_x = (v_x - r - x0) / xv;
    let mut high_x = (v_x + v_s + r - x0) / xv;
    if xv < 0.0 {
        std::mem::swap(&mut low_x, &mut high_x);
    }
    let mut low_y = (v_y - r - y0) / yv;
    let mut high_y = (v_y + v_s + r - y0) / yv;
    if yv < 0.0 {
        std::mem::swap(&mut low_y, &mut high_y);
    }
    ((xv == 0.0 && v_x <= x0 + r && x0 - r <= v_x + v_s)
        || (low_x <= 1.0 && 1.0 <= high_x)
        || (low_x <= 0.0 && 0.0 <= high_x)
        || (0.0 <= low_x && high_x <= 1.0))
        && ((yv == 0.0 && v_y <= y0 + r && y0 - r <= v_y + v_s)
            || ((low_y <= 1.0 && 1.0 <= high_y)
                || (low_y <= 0.0 && 0.0 <= high_y)
                || (0.0 <= low_y && high_y <= 1.0)))
        && (xv == 0.0
            || yv == 0.0
            || (low_y <= high_x && high_x <= high_y)
            || (low_y <= low_x && low_x <= high_y)
            || (low_x <= low_y && high_y <= high_x))
}

const HORIZONTAL: V2 = V2 {
    x: GOOD_VOXEL_SIZE,
    y: 0.0,
};
const VERTICAL: V2 = V2 {
    x: 0.0,
    y: GOOD_VOXEL_SIZE,
};

const NONE: usize = usize::MAX;

struct Bucket {
    head: usize,
    tail: usize,
    count: usize,
}

struct Entry {
    motion: Motion,
    next: usize,
}

struct Buckets {
    tree: Rbt<V2, usize>,
    buckets: Vec<Bucket>,
    entries: Vec<Entry>,
}

fn put_into_map(map: &mut Buckets, voxel: V2, motion: &Motion) {
    let node = map.tree.find_node(&voxel);
    let index = if node == NIL {
        let index = map.buckets.len();
        map.buckets.push(Bucket {
            head: NONE,
            tail: NONE,
            count: 0,
        });
        map.tree.put(voxel, index);
        index
    } else {
        map.tree.entries[node - 1].value
    };
    let entry = map.entries.len();
    map.entries.push(Entry {
        motion: *motion,
        next: NONE,
    });
    let bucket = &mut map.buckets[index];
    if bucket.head == NONE {
        bucket.head = entry;
    } else {
        map.entries[bucket.tail].next = entry;
    }
    bucket.tail = entry;
    bucket.count += 1;
}

fn recurse(voxel_map: &mut Buckets, seen: &mut Rbt<V2, bool>, next: V2, motion: &Motion) {
    if !is_in_voxel(next, motion) {
        return;
    }
    if seen.put(next, true) == Some(true) {
        return;
    }
    put_into_map(voxel_map, next, motion);
    recurse(voxel_map, seen, next.minus(HORIZONTAL), motion);
    recurse(voxel_map, seen, next.plus(HORIZONTAL), motion);
    recurse(voxel_map, seen, next.minus(VERTICAL), motion);
    recurse(voxel_map, seen, next.plus(VERTICAL), motion);
    recurse(
        voxel_map,
        seen,
        next.minus(HORIZONTAL).minus(VERTICAL),
        motion,
    );
    recurse(
        voxel_map,
        seen,
        next.minus(HORIZONTAL).plus(VERTICAL),
        motion,
    );
    recurse(
        voxel_map,
        seen,
        next.plus(HORIZONTAL).minus(VERTICAL),
        motion,
    );
    recurse(
        voxel_map,
        seen,
        next.plus(HORIZONTAL).plus(VERTICAL),
        motion,
    );
}

fn voxel_hash(p: V3) -> V2 {
    let x_div = (p.x / GOOD_VOXEL_SIZE) as i32;
    let y_div = (p.y / GOOD_VOXEL_SIZE) as i32;
    let mut x = GOOD_VOXEL_SIZE * x_div as f64;
    let mut y = GOOD_VOXEL_SIZE * y_div as f64;
    if p.x < 0.0 {
        x -= GOOD_VOXEL_SIZE;
    }
    if p.y < 0.0 {
        y -= GOOD_VOXEL_SIZE;
    }
    V2 { x, y }
}

fn reduce_collision_set(motions: &[Motion]) -> (Buckets, Vec<usize>) {
    let mut map = Buckets {
        tree: Rbt::new(),
        buckets: Vec::new(),
        entries: Vec::new(),
    };
    for motion in motions {
        let mut seen: Rbt<V2, bool> = Rbt::new();
        recurse(&mut map, &mut seen, voxel_hash(motion.pos_one), motion);
    }
    let mut result = Vec::new();
    map.tree.for_each(|_, &index| {
        if map.buckets[index].count > 1 {
            result.push(index);
        }
    });
    (map, result)
}

struct Detector {
    state: Rbt<i32, V3>,
}

impl Detector {
    fn handle_new_frame(&mut self, frame: &[Aircraft]) -> Vec<Collision> {
        let mut motions = Vec::new();
        let mut seen: Rbt<i32, bool> = Rbt::new();
        for a in frame {
            let old = self.state.put(a.callsign, a.position);
            let new = a.position;
            seen.put(a.callsign, true);
            motions.push(Motion {
                callsign: a.callsign,
                pos_one: old.unwrap_or(new),
                pos_two: new,
            });
        }
        let mut to_remove = Vec::new();
        self.state.for_each(|k, _| {
            if seen.get(k) != Some(true) {
                to_remove.push(*k);
            }
        });
        for k in to_remove {
            self.state.remove(&k);
        }
        let (map, reduced) = reduce_collision_set(&motions);
        let mut collisions = Vec::new();
        for bucket in reduced {
            let mut first = map.buckets[bucket].head;
            while first != NONE {
                let m1 = &map.entries[first].motion;
                let mut second = map.entries[first].next;
                while second != NONE {
                    let m2 = &map.entries[second].motion;
                    if let Some(c) = m1.find_intersection(m2) {
                        collisions.push(Collision {
                            a: m1.callsign,
                            b: m2.callsign,
                            position: c,
                        });
                    }
                    second = map.entries[second].next;
                }
                first = map.entries[first].next;
            }
        }
        collisions
    }
}

fn simulate(n: i32, time: f64) -> Vec<Aircraft> {
    let mut frame = Vec::new();
    let mut i = 0;
    while i < n {
        frame.push(Aircraft {
            callsign: i,
            position: V3 {
                x: time,
                y: time.cos() * 2.0 + (i * 3) as f64,
                z: 10.0,
            },
        });
        frame.push(Aircraft {
            callsign: i + 1,
            position: V3 {
                x: time,
                y: time.sin() * 2.0 + (i * 3) as f64,
                z: 10.0,
            },
        });
        i += 2;
    }
    frame
}

fn benchmark(n: i32) -> i32 {
    let mut d = Detector { state: Rbt::new() };
    let mut actual = 0;
    for i in 0..200 {
        let time = i as f64 / 10.0;
        actual += d.handle_new_frame(&simulate(n, time)).len() as i32;
    }
    actual
}

pub fn inner(inner: i32) -> bool {
    let c = benchmark(inner);
    match inner {
        1000 => c == 14484,
        500 => c == 14484,
        250 => c == 10830,
        200 => c == 8655,
        100 => c == 4305,
        10 => c == 390,
        2 => c == 42,
        _ => {
            println!("No verification result for {} found", inner);
            println!("Result is: {}", c);
            false
        }
    }
}
