//! Finite instance closure (LANG-12). Parameter flow, rather than argument equality,
//! distinguishes a finite permutation/reset from constructor growth fed back to itself.
use crate::*;
use hirsel_core_resolve::Site;

#[derive(Clone, Copy)]
struct Edge {
    from: usize,
    to: usize,
    growing: bool,
    site: Site,
}

impl Program<'_> {
    pub(crate) fn check_recursive_instances(&mut self) {
        let base = self.decls.base_len();
        let mut offsets = vec![0];
        let mut nodes = vec![];
        for d in base..self.decls.len() {
            nodes.extend((0..self.decls[d].generics.len()).map(|i| (d, i)));
            offsets.push(nodes.len());
        }
        if nodes.is_empty() {
            return;
        }
        let mut edges = vec![];
        let mut todo = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut argument = vec![];
        let mut argument_seen = std::collections::HashSet::new();
        for d in base..self.decls.len() {
            let decl = &self.decls[d];
            if decl.generics.is_empty() {
                continue;
            }
            let removed = decl
                .codec()
                .into_iter()
                .flat_map(|c| &c.removed)
                .flat_map(|r| &r.fields);
            for field in decl.all_fields().chain(removed) {
                todo.clear();
                seen.clear();
                todo.push(field.ty);
                while let Some(ty) = todo.pop() {
                    if !seen.insert(ty) {
                        continue;
                    }
                    let kind = self.types.kind(ty);
                    children(kind, &mut todo);
                    let TyKind::Adt(e, xs) = kind else { continue };
                    // A checked published library cannot refer to a new declaration.
                    // Nested references in its arguments are still walked above.
                    if e.index() < base {
                        continue;
                    }
                    for (j, &x) in xs.iter().enumerate() {
                        if !crate::types::has_param(&self.types, x) {
                            continue;
                        }
                        argument.clear();
                        argument_seen.clear();
                        argument.push((x, false));
                        while let Some((a, growing)) = argument.pop() {
                            if !argument_seen.insert((a, growing)) {
                                continue;
                            }
                            let kind = self.types.kind(a);
                            match kind {
                                TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
                                    edges.push(Edge {
                                        from: offsets[d - base] + *i as usize,
                                        to: offsets[e.index() - base] + j,
                                        growing,
                                        site: field.site,
                                    });
                                }
                                _ => {
                                    // These are actual type constructors. Aliases were
                                    // expanded by named_ty before this check.
                                    let start = todo.len();
                                    children(kind, &mut todo);
                                    argument.extend(todo.drain(start..).map(|a| (a, true)));
                                }
                            }
                        }
                    }
                }
            }
        }
        let (components, outgoing) = components(nodes.len(), &edges);
        let mut growing = vec![None; nodes.len()];
        for (i, edge) in edges.iter().enumerate() {
            if edge.growing && components[edge.from] == components[edge.to] {
                growing[components[edge.from]].get_or_insert(i);
            }
        }
        let mut reported = std::collections::HashSet::new();
        let mut messages = std::collections::HashMap::new();
        for edge in &edges {
            let c = components[edge.from];
            if c != components[edge.to] {
                continue;
            }
            let Some(first) = growing[c] else { continue };
            self.decls[nodes[edge.from].0].summary.cycle = true;
            if !reported.insert((
                edge.site.module,
                edge.site.item,
                edge.site.span.start,
                edge.site.span.end,
            )) {
                continue;
            }
            let message = messages.entry(c).or_insert_with(|| {
                let first = edges[first];
                let mut previous = vec![None; nodes.len()];
                let mut pending = vec![first.to];
                previous[first.to] = Some(first.to);
                while let Some(v) = pending.pop() {
                    if v == first.from {
                        break;
                    }
                    for &i in &outgoing[v] {
                        let w = edges[i].to;
                        if components[w] == c && previous[w].is_none() {
                            previous[w] = Some(v);
                            pending.push(w);
                        }
                    }
                }
                let mut path = vec![first.from];
                while *path.last().unwrap() != first.to {
                    path.push(previous[*path.last().unwrap()].unwrap());
                }
                path.reverse();
                path.insert(0, first.from);
                let names = path
                    .into_iter()
                    .map(|v| {
                        let (d, i) = nodes[v];
                        format!(
                            "{}[{}]",
                            self.decls[d].path,
                            self.index.symbols.text(self.decls[d].generics[i].name)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" -> ");
                format!("recursive type instances grow without bound along {names}")
            });
            self.refusals
                .push(refusal("LANG-12", edge.site, message.clone()));
        }
    }
}

fn children(kind: &TyKind, into: &mut Vec<TyId>) {
    match kind {
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => into.extend(xs),
        TyKind::Array(x, _) | TyKind::View(x) => into.push(*x),
        TyKind::Anchored(x, r) => into.extend([*x, *r]),
        TyKind::Fn(ps, r, _, _) => {
            into.extend(ps.iter().map(|p| p.1));
            into.push(*r);
        }
        _ => {}
    }
}

/// Iterative Kosaraju: no recursion or depth/instance cutoff. An internal growing
/// edge in a strongly connected component is exactly an unbounded substitution.
fn components(n: usize, edges: &[Edge]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let mut outgoing = vec![vec![]; n];
    let mut reverse = vec![vec![]; n];
    for (i, edge) in edges.iter().enumerate() {
        outgoing[edge.from].push(i);
        reverse[edge.to].push(edge.from);
    }
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut stack = vec![];
    for v in 0..n {
        if seen[v] {
            continue;
        }
        seen[v] = true;
        stack.push((v, 0));
        while let Some((v, next)) = stack.last_mut() {
            if let Some(&i) = outgoing[*v].get(*next) {
                *next += 1;
                let w = edges[i].to;
                if !seen[w] {
                    seen[w] = true;
                    stack.push((w, 0));
                }
            } else {
                order.push(*v);
                stack.pop();
            }
        }
    }
    let mut groups = vec![usize::MAX; n];
    let mut pending = vec![];
    for &v in order.iter().rev() {
        if groups[v] != usize::MAX {
            continue;
        }
        groups[v] = v;
        pending.push(v);
        while let Some(w) = pending.pop() {
            for &u in &reverse[w] {
                if groups[u] == usize::MAX {
                    groups[u] = v;
                    pending.push(u);
                }
            }
        }
    }
    (groups, outgoing)
}
