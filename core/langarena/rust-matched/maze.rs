// Reviewed-by: langs-numeric-920 2026-11-13; Generator/BFS/AStar: same nested cells, shuffled inline neighbors, flat BFS marks, i32 path/scores, min priorities and LCG. Overflow checks required.
// BFS/AStar checked path conversions and inline neighbor copies now mirror Hirsel; delta approved by langs-numeric-920 2026-11-13 at b67a67e0; scoped to changed conversion/copy sites.
// Remaining shuffle and LCG conversion parity approved by langs-numeric-920 2026-11-13; remaining as sites are lossless widening or checksum truncation.
// Matched twin of maze.hirsel. Reviewed below.
// kostya/LangArena3c9f84d benchmarks/maze.rs:8-468.
// Nested cell vectors, four ordered neighbors, u32 kinds, same path arrays and LCG.
mod text_support;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
fn walkable(k: u32) -> bool {
    k == 1 || k == 2 || k == 3
}
fn next(g: &mut i32, max: i32) -> i32 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    let t = (f64::from(*g) / 139968.0 * f64::from(max)).trunc();
    assert!(t >= -2147483648.0 && t < 2147483648.0, "float.to_int i32");
    t as i32
}
fn heuristic(a: (usize, usize), b: (usize, usize)) -> i32 {
    i32::try_from(a.0.abs_diff(b.0) + a.1.abs_diff(b.1)).unwrap()
}
pub struct Cell {
    kind: u32,
    neighbors: [(usize, usize); 4],
    pub x: usize,
    pub y: usize,
}

impl Cell {
    fn new(x: usize, y: usize) -> Self {
        Self {
            kind: 0u32,
            neighbors: [(0, 0); 4],
            x,
            y,
        }
    }

    fn reset(&mut self) {
        if self.kind == 1u32 {
            self.kind = 0u32;
        }
    }
}

pub struct Maze {
    width: usize,
    height: usize,
    cells: Vec<Vec<Cell>>,
}

impl Maze {
    pub fn new(width: usize, height: usize, g: &mut i32) -> Self {
        let width = width.max(5);
        let height = height.max(5);

        let mut cells = Vec::with_capacity(height);
        for y in 0..height {
            let mut row = Vec::with_capacity(width);
            for x in 0..width {
                row.push(Cell::new(x, y));
            }
            cells.push(row);
        }

        let mut maze = Maze {
            width,
            height,
            cells,
        };

        maze.link_neighbors(g);
        maze
    }

    fn link_neighbors(&mut self, g: &mut i32) {
        for y in 0..self.height {
            for x in 0..self.width {
                if x == 0 || y == 0 || x == self.width - 1 || y == self.height - 1 {
                    self.cells[y][x].kind = 4u32;
                } else {
                    let mut neighbors = [(y - 1, x), (y + 1, x), (y, x + 1), (y, x - 1)];

                    for _ in 0..4 {
                        let i = usize::try_from(next(g, 4)).unwrap();
                        let j = usize::try_from(next(g, 4)).unwrap();
                        if i != j {
                            neighbors.swap(i, j);
                        }
                    }

                    self.cells[y][x].neighbors = neighbors;
                }
            }
        }

        self.cells[1][1].kind = 2u32;
        self.cells[self.height - 2][self.width - 2].kind = 3u32;
    }

    pub fn reset(&mut self) {
        for row in &mut self.cells {
            for cell in row {
                cell.reset();
            }
        }
        self.cells[1][1].kind = 2u32;
        self.cells[self.height - 2][self.width - 2].kind = 3u32;
    }

    fn dig(&mut self, start: (usize, usize)) {
        let mut stack = Vec::with_capacity(self.width * self.height);
        stack.push(start);

        while let Some((y, x)) = stack.pop() {
            let walkable_neighbors = self.cells[y][x]
                .neighbors
                .iter()
                .filter(|&&(ny, nx)| walkable(self.cells[ny][nx].kind))
                .count();

            if walkable_neighbors == 1 {
                self.cells[y][x].kind = 1u32;

                for &(ny, nx) in &self.cells[y][x].neighbors {
                    if self.cells[ny][nx].kind == 0u32 {
                        stack.push((ny, nx));
                    }
                }
            }
        }
    }

    fn ensure_open_finish(&mut self, pos: (usize, usize)) {
        let (y, x) = pos;
        self.cells[y][x].kind = 1u32;

        let neighbors_to_check: Vec<(usize, usize)> = self.cells[y][x]
            .neighbors
            .iter()
            .filter(|&&(ny, nx)| self.cells[ny][nx].kind == 0u32)
            .copied()
            .collect();

        let walkable_neighbors = self.cells[y][x]
            .neighbors
            .iter()
            .filter(|&&(ny, nx)| walkable(self.cells[ny][nx].kind))
            .count();

        if walkable_neighbors > 1 {
            return;
        }

        for neighbor_pos in neighbors_to_check {
            self.ensure_open_finish(neighbor_pos);
        }
    }

    pub fn generate(&mut self) {
        let start_neighbors = self.cells[1][1].neighbors;

        for &(ny, nx) in &start_neighbors {
            if self.cells[ny][nx].kind == 0u32 {
                self.dig((ny, nx));
            }
        }

        let finish_pos = (self.height - 2, self.width - 2);
        let finish_neighbors = self.cells[finish_pos.0][finish_pos.1].neighbors;

        for &(ny, nx) in &finish_neighbors {
            if self.cells[ny][nx].kind == 0u32 {
                self.ensure_open_finish((ny, nx));
            }
        }
    }

    pub fn middle_cell(&self) -> &Cell {
        &self.cells[self.height / 2][self.width / 2]
    }

    pub fn checksum(&self) -> u32 {
        let mut hasher = 2166136261u32;
        let prime = 16777619u32;

        for row in &self.cells {
            for cell in row {
                if cell.kind == 1u32 {
                    let val = (cell.x * cell.y) as u32;
                    hasher = (hasher ^ val).wrapping_mul(prime);
                }
            }
        }
        hasher
    }

    pub fn get_start(&self) -> (usize, usize) {
        (1, 1)
    }

    pub fn get_finish(&self) -> (usize, usize) {
        (self.height - 2, self.width - 2)
    }

    pub fn get_cell(&self, y: usize, x: usize) -> &Cell {
        &self.cells[y][x]
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
}

fn bfs(m: &Maze, start: (usize, usize), target: (usize, usize)) -> Vec<(usize, usize)> {
    if start == target {
        return vec![start];
    }

    let width = m.width();
    let height = m.height();

    let mut queue = VecDeque::new();
    let mut visited = vec![false; width * height];
    let mut path = Vec::new();

    visited[start.0 * width + start.1] = true;
    path.push((start.0, start.1, -1i32));
    queue.push_back(0);

    while let Some(path_id) = queue.pop_front() {
        let (y, x, _) = path[path_id];

        let neighbors = m.get_cell(y, x).neighbors;
        for (ny, nx) in neighbors {
            if (ny, nx) == target {
                let mut result = vec![target];
                let mut current = i32::try_from(path_id).unwrap();
                while current >= 0 {
                    let (py, px, _) = path[usize::try_from(current).unwrap()];
                    result.push((py, px));
                    current = path[usize::try_from(current).unwrap()].2;
                }
                result.reverse();
                return result;
            }

            if walkable(m.get_cell(ny, nx).kind) && !visited[ny * width + nx] {
                visited[ny * width + nx] = true;
                path.push((ny, nx, i32::try_from(path_id).unwrap()));
                queue.push_back(path.len() - 1);
            }
        }
    }

    Vec::new()
}
fn astar(m: &Maze, start: (usize, usize), target: (usize, usize)) -> Vec<(usize, usize)> {
    if start == target {
        return vec![start];
    }

    let width = m.width();
    let height = m.height();
    let size = width * height;

    let idx = |y: usize, x: usize| y * width + x;

    let start_idx = idx(start.0, start.1);
    let target_idx = idx(target.0, target.1);

    let mut came_from = vec![-1i32; size];
    let mut g_score = vec![i32::MAX; size];
    let mut best_f = vec![i32::MAX; size];

    let mut open_set = BinaryHeap::new();

    g_score[start_idx] = 0;
    let f_start = heuristic(start, target);
    open_set.push(Reverse((f_start, start_idx)));
    best_f[start_idx] = f_start;

    while let Some(Reverse((_, current_idx))) = open_set.pop() {
        if current_idx == target_idx {
            let mut path = Vec::new();
            let mut cur = i32::try_from(current_idx).unwrap();
            while cur != -1 {
                let y = (usize::try_from(cur).unwrap()) / width;
                let x = (usize::try_from(cur).unwrap()) % width;
                path.push((y, x));
                cur = came_from[usize::try_from(cur).unwrap()];
            }
            path.reverse();
            return path;
        }

        let current_y = current_idx / width;
        let current_x = current_idx % width;
        let current_g = g_score[current_idx];

        let neighbors = m.get_cell(current_y, current_x).neighbors;
        for (ny, nx) in neighbors {
            if !walkable(m.get_cell(ny, nx).kind) {
                continue;
            }

            let neighbor_idx = idx(ny, nx);
            let tentative_g = current_g + 1;

            if tentative_g < g_score[neighbor_idx] {
                came_from[neighbor_idx] = i32::try_from(current_idx).unwrap();
                g_score[neighbor_idx] = tentative_g;
                let f_new = tentative_g + heuristic((ny, nx), target);

                if f_new < best_f[neighbor_idx] {
                    best_f[neighbor_idx] = f_new;
                    open_set.push(Reverse((f_new, neighbor_idx)));
                }
            }
        }
    }

    Vec::new()
}
fn main() {
    let a: Vec<String> = text_support::arguments(&["w", "h"], true);
    let task = &a[1];
    let width: usize = a[2].parse().unwrap();
    let height: usize = a[3].parse().unwrap();
    let warmup: u64 = a[4].parse().unwrap();
    let iterations: u64 = a[5].parse().unwrap();
    let mut m = Maze::new(width, height, &mut 42);
    if task != "Maze::Generator" {
        m.generate();
    }
    let mut path = Vec::new();
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        if task == "Maze::Generator" {
            m.reset();
            m.generate();
            checksum = checksum.wrapping_add(m.middle_cell().kind);
        } else {
            path = match task.as_str() {
                "Maze::BFS" => bfs(&m, m.get_start(), m.get_finish()),
                "Maze::AStar" => astar(&m, m.get_start(), m.get_finish()),
                _ => panic!("unknown task"),
            };
            checksum = checksum.wrapping_add(u32::try_from(path.len()).unwrap());
        }
    }
    let final_part = if task == "Maze::Generator" {
        m.checksum()
    } else if path.is_empty() {
        0
    } else {
        let (y, x) = path[path.len() / 2];
        (x * y) as u32
    };
    text_support::output(checksum.wrapping_add(final_part));
}
