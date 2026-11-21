// AWFY list.js and towers.js. Links index one grow-only pool per invocation.
const NONE: usize = usize::MAX;

struct Element {
    value: i64,
    next: usize,
}

fn make_list(pool: &mut Vec<Element>, size: i64) -> usize {
    if size == 0 {
        return NONE;
    }
    let node = pool.len();
    pool.push(Element {
        value: size,
        next: NONE,
    });
    let rest = make_list(pool, size - 1);
    pool[node].next = rest;
    node
}

fn shorter(pool: &[Element], mut left: usize, mut right: usize) -> bool {
    while right != NONE {
        if left == NONE {
            return true;
        }
        left = pool[left].next;
        right = pool[right].next;
    }
    false
}

fn tail(pool: &[Element], x: usize, y: usize, z: usize) -> usize {
    if !shorter(pool, y, x) {
        return z;
    }
    if x == NONE || y == NONE || z == NONE {
        return NONE;
    }
    tail(
        pool,
        tail(pool, pool[x].next, y, z),
        tail(pool, pool[y].next, z, x),
        tail(pool, pool[z].next, x, y),
    )
}

fn length(pool: &[Element], node: usize) -> usize {
    if node == NONE {
        0
    } else {
        1 + length(pool, pool[node].next)
    }
}

pub fn list(first: i64) -> usize {
    let mut pool = Vec::new();
    let x = make_list(&mut pool, first);
    let y = make_list(&mut pool, 10);
    let z = make_list(&mut pool, 6);
    length(&pool, tail(&pool, x, y, z))
}

struct Towers {
    piles: [usize; 3],
    disks: Vec<Element>,
    moves: usize,
}

impl Towers {
    fn push(&mut self, disk: usize, pile: usize) {
        let top = self.piles[pile];
        assert!(top == NONE || self.disks[disk].value < self.disks[top].value);
        self.disks[disk].next = top;
        self.piles[pile] = disk;
    }

    fn move_top(&mut self, from: usize, to: usize) {
        let disk = self.piles[from];
        assert_ne!(disk, NONE);
        self.piles[from] = self.disks[disk].next;
        self.disks[disk].next = NONE;
        self.push(disk, to);
        self.moves += 1;
    }

    fn move_disks(&mut self, count: usize, from: usize, to: usize) {
        if count == 1 {
            self.move_top(from, to);
        } else {
            let other = 3 - from - to;
            self.move_disks(count - 1, from, other);
            self.move_top(from, to);
            self.move_disks(count - 1, other, to);
        }
    }
}

pub fn towers(count: usize) -> usize {
    let mut towers = Towers {
        piles: [NONE; 3],
        disks: Vec::new(),
        moves: 0,
    };
    for size in (0..=count).rev() {
        let disk = towers.disks.len();
        towers.disks.push(Element {
            value: size as i64,
            next: NONE,
        });
        towers.push(disk, 0);
    }
    towers.move_disks(count, 0, 1);
    towers.moves
}
