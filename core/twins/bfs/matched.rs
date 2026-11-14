// Matched to core/programs/m1/bfs/matched.hirsel. Independent review pending.
#[path = "../matched_support.rs"]
mod support;
use support::*;
fn run(input: View<'_>, variant: &str, trace: &Trace) -> Result<()> {
    let mut at = 0;
    let header = line(input, &mut at);
    let (ns, rest) = header.split_once(' ').ok_or("Parse@graph")?;
    let (fs, ts) = rest.split_once(' ').ok_or("Parse@graph")?;
    let n = number(ns)?;
    let from = number(fs)?;
    let to = number(ts)?;
    let _graph = trace.marker("graph");
    if from >= n || to >= n {
        return Err("InvalidVertex".into());
    }
    let start = at;
    while at < input.len() {
        let (a, b) = line(input, &mut at).split_once(' ').ok_or("Parse@edge")?;
        if number(a)? >= n || number(b)? >= n {
            return Err("InvalidVertex".into());
        }
    }
    let mut offsets = Vec::with_capacity(index(n + 1));
    offsets.resize(index(n + 1), 0u32);
    let mut edge_count = 0usize;
    at = start;
    while at < input.len() {
        let (a, _) = line(input, &mut at).split_once(' ').ok_or("Parse@edge")?;
        let u = number(a)? + 1;
        offsets[index(u)] += 1;
        edge_count += 1;
    }
    let mut i = 1usize;
    while i <= index(n) {
        offsets[i] += offsets[i - 1];
        i += 1;
    }
    let mut positions = Vec::with_capacity(index(n));
    for i in 0..index(n) {
        positions.push(offsets[i]);
    }
    let mut neighbours = Vec::with_capacity(edge_count);
    neighbours.resize(edge_count, 0u32);
    at = start;
    while at < input.len() {
        let (a, b) = line(input, &mut at).split_once(' ').ok_or("Parse@edge")?;
        let u = index(number(a)?);
        let pos = positions[u] as usize;
        neighbours[pos] = u32::try_from(number(b)?).unwrap();
        positions[u] += 1;
    }
    admission(variant, "search.parent", 8 * n)?;
    let _parent = trace.marker("parent");
    let mut parent = Vec::with_capacity(index(n));
    parent.resize(index(n), n);
    admission(variant, "search.queue", 8 * n)?;
    let _queue = trace.marker("queue");
    let mut queue = Vec::with_capacity(index(n));
    queue.push(from);
    parent[index(from)] = from;
    let mut head = 0usize;
    while head < queue.len() {
        let here = index(queue[head]);
        head += 1;
        let mut lo = offsets[here] as usize;
        let hi = offsets[here + 1] as usize;
        while lo < hi {
            let next = u64::from(neighbours[lo]);
            if parent[index(next)] == n {
                parent[index(next)] = here as u64;
                queue.push(next);
            }
            lo += 1;
        }
    }
    if parent[index(to)] == n {
        return Err("NoPath".into());
    }
    let mut length = 1usize;
    let mut cursor = to;
    while cursor != from {
        cursor = parent[index(cursor)];
        length += 1;
    }
    admission(variant, "result.path", (length as u64) * 8)?;
    let mut path = Vec::with_capacity(length);
    cursor = to;
    path.push(cursor);
    while cursor != from {
        cursor = parent[index(cursor)];
        path.push(cursor);
    }
    path.reverse();
    let mut out = String::new();
    out.push_str("path=");
    let mut first = true;
    for node in path.iter() {
        if !first {
            out.push(',');
        }
        first = false;
        num(&mut out, *node);
    }
    out.push_str("\nvisited=");
    num(&mut out, queue.len());
    out.push('\n');
    put(&out)
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let trace = Trace::new(6);
    let result = run(input.view(), &variant, &trace);
    trace.finish(result);
}
