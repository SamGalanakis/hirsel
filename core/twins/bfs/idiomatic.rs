mod program;
#[path = "../support.rs"]
mod support;
struct Graph(Vec<Vec<usize>>);
impl program::Graph for Graph {
    fn build(n: usize, edges: &[(usize, usize)]) -> Self {
        let mut adj = vec![Vec::new(); n];
        for &(a, b) in edges {
            adj[a].push(b);
        }
        Self(adj)
    }
    fn visit(&self, v: usize, f: impl FnMut(usize)) {
        self.0[v].iter().copied().for_each(f);
    }
}
fn main() {
    program::run::<Graph>();
}
