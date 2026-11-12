mod program;
#[path = "../support.rs"]
mod support;
struct Graph {
    offsets: Vec<u32>,
    edges: Vec<u32>,
}
impl program::Graph for Graph {
    fn build(n: usize, edges: &[(usize, usize)]) -> Self {
        let mut offsets = vec![0u32; n + 1];
        for &(a, _) in edges {
            offsets[a + 1] += 1;
        }
        for i in 1..=n {
            offsets[i] += offsets[i - 1];
        }
        let mut cursor = offsets.clone();
        let mut out = vec![0; edges.len()];
        for &(a, b) in edges {
            out[cursor[a] as usize] = u32::try_from(b).unwrap();
            cursor[a] += 1;
        }
        Self {
            offsets,
            edges: out,
        }
    }
    fn visit(&self, v: usize, mut f: impl FnMut(usize)) {
        for &u in &self.edges[self.offsets[v] as usize..self.offsets[v + 1] as usize] {
            f(u as usize);
        }
    }
}
fn main() {
    program::run::<Graph>();
}
