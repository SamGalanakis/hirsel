// Reviewed-by: langs-numeric-920 2026-11-13; BFS/DFS/AStar preserve nested i32 adjacency, byte bool marks, FIFO/LIFO/min-heap priorities, LCG and checked arithmetic; overflow checks required.
// Matched twin of graph.hirsel. Reviewed below.
// kostya/LangArena3c9f84d benchmarks/graph_path.rs:11-316.
// Adjacency vectors, FIFO/LIFO searches, minimum (f,vertex) heap and explicit LCG.
mod text_support;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, VecDeque};
pub struct Graph {
    pub vertices: i32,
    pub jumps: i32,
    pub jump_len: i32,
    pub adj: Vec<Vec<i32>>,
}

impl Graph {
    pub fn new(vertices: i32, jumps: i32, jump_len: i32) -> Self {
        let mut adj = Vec::with_capacity(vertices as usize);
        for _ in 0..vertices {
            adj.push(Vec::new());
        }

        Self {
            vertices,
            jumps,
            jump_len,
            adj,
        }
    }

    pub fn add_edge(&mut self, u: i32, v: i32) {
        let u_idx = u as usize;
        let v_idx = v as usize;

        if u_idx < self.adj.len() && v_idx < self.adj.len() {
            self.adj[u_idx].push(v);
            self.adj[v_idx].push(u);
        }
    }

    pub fn generate_random(&mut self, g: &mut i32) {
        for i in 1..self.vertices {
            self.add_edge(i, i - 1);
        }

        for v in 0..self.vertices {
            let num_jumps = next(g, self.jumps as i32) as i32;
            for _ in 0..num_jumps {
                let offset = next(g, self.jump_len as i32) - self.jump_len / 2;
                let u = v + offset;

                if u >= 0 && u < self.vertices && u != v {
                    self.add_edge(v, u);
                }
            }
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct Node {
    f_score: i32,
    vertex: i32,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        self.f_score
            .cmp(&other.f_score)
            .then_with(|| self.vertex.cmp(&other.vertex))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn bfs_shortest_path(graph: &Graph, start: i32, target: i32) -> i64 {
    if start == target {
        return 0;
    }

    let vertices = graph.vertices as usize;
    let mut visited = vec![false; vertices];
    let mut queue = VecDeque::new();

    visited[start as usize] = true;
    queue.push_back((start, 0));

    while let Some((v, dist)) = queue.pop_front() {
        let v_idx = v as usize;

        for &neighbor in &graph.adj[v_idx] {
            if neighbor == target {
                return (dist + 1) as i64;
            }

            let neighbor_idx = neighbor as usize;
            if !visited[neighbor_idx] {
                visited[neighbor_idx] = true;
                queue.push_back((neighbor, dist + 1));
            }
        }
    }

    -1
}
fn dfs_find_path(graph: &Graph, start: i32, target: i32) -> i64 {
    if start == target {
        return 0;
    }

    let vertices = graph.vertices as usize;
    let mut visited = vec![false; vertices];
    let mut stack = vec![(start, 0)];
    let mut best_path = i32::MAX;

    while let Some((v, dist)) = stack.pop() {
        if visited[v as usize] || dist >= best_path {
            continue;
        }
        visited[v as usize] = true;

        for &neighbor in &graph.adj[v as usize] {
            if neighbor == target {
                if dist + 1 < best_path {
                    best_path = dist + 1;
                }
            } else if !visited[neighbor as usize] {
                stack.push((neighbor, dist + 1));
            }
        }
    }

    if best_path == i32::MAX {
        -1
    } else {
        best_path as i64
    }
}
fn a_star_shortest_path(graph: &Graph, start: i32, target: i32) -> i64 {
    if start == target {
        return 0;
    }

    let vertices = graph.vertices as usize;

    let mut g_score = vec![i32::MAX; vertices];
    let mut best_f = vec![i32::MAX; vertices];

    g_score[start as usize] = 0;
    let f_start = target - start;
    best_f[start as usize] = f_start;

    let mut open_set = BinaryHeap::new();
    open_set.push(Reverse(Node {
        f_score: f_start,
        vertex: start,
    }));

    while let Some(Reverse(current)) = open_set.pop() {
        if current.vertex == target {
            return g_score[current.vertex as usize] as i64;
        }

        for &neighbor in &graph.adj[current.vertex as usize] {
            let tentative_g = g_score[current.vertex as usize] + 1;

            if tentative_g < g_score[neighbor as usize] {
                g_score[neighbor as usize] = tentative_g;
                let f_new = tentative_g + (target - neighbor);

                if f_new < best_f[neighbor as usize] {
                    best_f[neighbor as usize] = f_new;
                    open_set.push(Reverse(Node {
                        f_score: f_new,
                        vertex: neighbor,
                    }));
                }
            }
        }
    }

    -1
}
fn next(g: &mut i32, max: i32) -> i32 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    (*g as f64 / 139968.0 * f64::from(max)) as i32
}
fn main() {
    let a: Vec<String> = text_support::arguments(&["vertices", "jumps", "jump_len"], true);
    let task = &a[1];
    let vertices: i32 = a[2].parse().unwrap();
    let jumps: i32 = a[3].parse().unwrap();
    let jump_len: i32 = a[4].parse().unwrap();
    let warmup: u64 = a[5].parse().unwrap();
    let iterations: u64 = a[6].parse().unwrap();
    let mut graph = Graph::new(vertices, jumps, jump_len);
    graph.generate_random(&mut 42);
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        let d = match task.as_str() {
            "Graph::BFS" => bfs_shortest_path(&graph, 0, vertices - 1),
            "Graph::DFS" => dfs_find_path(&graph, 0, vertices - 1),
            "Graph::AStar" => a_star_shortest_path(&graph, 0, vertices - 1),
            _ => panic!("unknown task"),
        };
        checksum = checksum.wrapping_add(d as u32);
    }
    text_support::output(checksum);
}
