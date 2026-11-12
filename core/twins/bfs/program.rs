use crate::support::*;
pub trait Graph {
    fn build(n: usize, edges: &[(usize, usize)]) -> Self;
    fn visit(&self, v: usize, f: impl FnMut(usize));
}
pub fn run<G: Graph>() {
    let (input, variant) = input();
    let mut lines = input.lines();
    let header: Vec<usize> = lines
        .next()
        .unwrap()
        .split_whitespace()
        .map(|x| x.parse().unwrap())
        .collect();
    let (n, from, to) = (header[0], header[1], header[2]);
    let edges: Vec<(usize, usize)> = lines
        .map(|s| {
            let (a, b) = s.split_once(' ').unwrap();
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();
    let trace = Trace::default();
    let admission = Admission::new(&variant);
    let result = (|| {
        let _graph = Resource::new("graph", &trace);
        if from >= n || to >= n || edges.iter().any(|&(a, b)| a >= n || b >= n) {
            return Err("InvalidVertex".into());
        }
        let graph = G::build(n, &edges);
        admission.at("search.parent")?;
        let _parent = Resource::new("parent", &trace);
        let mut parent = vec![usize::MAX; n];
        admission.at("search.queue")?;
        let _queue = Resource::new("queue", &trace);
        let mut queue = Vec::with_capacity(n);
        queue.push(from);
        parent[from] = from;
        let mut head = 0;
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            graph.visit(v, |u| {
                if parent[u] == usize::MAX {
                    parent[u] = v;
                    queue.push(u);
                }
            });
        }
        if parent[to] == usize::MAX {
            return Err("NoPath".into());
        }
        admission.at("result.path")?;
        let mut path = vec![to];
        while *path.last().unwrap() != from {
            path.push(parent[*path.last().unwrap()]);
        }
        path.reverse();
        println!(
            "path={}",
            path.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        println!("visited={}", queue.len());
        Ok(())
    })();
    trace.print();
    finish(result);
}
