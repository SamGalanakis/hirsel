use super::own::{Act, D, F, Own, T, Z, begun, dropped, halves};
use super::own_names::{Pl, overlap, ptr_args};
use super::*;
use crate::decode::Debit;

impl Own<'_> {
    pub(super) fn solve(&self, m: &Module, width: usize, scratch: &Debit<'_>) -> Result<(), Error> {
        let b = self.b;
        let insts = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        let blocks = b.blocks.len();
        let mut start: Vec<Option<Vec<u8>>> = Vec::new();
        scratch.reserve(&mut start, blocks)?;
        start.resize_with(blocks, || None);
        if blocks > 0 {
            let mut init = Vec::new();
            scratch.reserve(&mut init, width)?;
            init.resize(width, Z);
            // Borrows are inactive until their call runs; flag reads are stale until read.
            for slot in &mut init[self.base..self.stale_base] {
                *slot = 0;
            }
            for slot in &mut init[self.stale_base..self.dest_escaped] {
                *slot = 1;
            }
            if self.has_dest {
                init[self.dest_escaped] = 0;
            }
            start[0] = Some(init);
        }
        let mut queued = Vec::new();
        scratch.reserve(&mut queued, blocks)?;
        queued.resize(blocks, false);
        let mut work = Vec::new();
        scratch.reserve(&mut work, blocks)?;
        if blocks > 0 {
            work.push(0usize);
            queued[0] = true;
        }
        let mut seen = Vec::new();
        scratch.reserve(&mut seen, self.tracked.len())?;
        let mut acts = Vec::new();
        let max_args = b
            .calls
            .iter()
            .map(|site| site.args.len as usize)
            .max()
            .unwrap_or(0);
        scratch.reserve(&mut acts, (max_args + 1).max(3))?;
        let mut now = Vec::new();
        scratch.reserve(&mut now, width)?;
        now.resize(width, 0);
        let mut edge_buf = Vec::new();
        scratch.reserve(&mut edge_buf, width)?;
        edge_buf.resize(width, 0);
        let send = |start: &mut Vec<Option<Vec<u8>>>,
                    queued: &mut [bool],
                    work: &mut Vec<usize>,
                    t: usize,
                    s: &[u8]|
         -> Result<(), Error> {
            scratch.check()?;
            let changed = if let Some(old) = &mut start[t] {
                let mut changed = false;
                for (x, y) in old.iter_mut().zip(s) {
                    let next = *x | *y;
                    changed |= *x != next;
                    *x = next;
                }
                changed
            } else {
                let mut next = Vec::new();
                scratch.reserve(&mut next, s.len())?;
                next.extend_from_slice(s);
                start[t] = Some(next);
                true
            };
            if changed && !queued[t] {
                queued[t] = true;
                work.push(t);
            }
            Ok(())
        };
        let mut step_block = |start: &mut Vec<Option<Vec<u8>>>,
                              queued: &mut [bool],
                              work: &mut Vec<usize>,
                              check: bool,
                              bi: usize|
         -> Result<(), Error> {
            scratch.check()?;
            now.copy_from_slice(start[bi].as_ref().unwrap());
            for i in insts(bi) {
                scratch.check()?;
                self.acts(m, i, &mut acts);
                let fail = i.fail(b);
                if !check && let (Some(f), false) = (fail, matches!(i, Inst::Drop { .. })) {
                    send(start, queued, work, f.index(), &now)?;
                }
                for a in &acts {
                    self.apply(m, &mut now, a, check, &mut seen)
                        .map_err(|e| Error::Invalid(format!("b{bi}: {e}")))?;
                }
                if !check && let (Some(f), true) = (fail, matches!(i, Inst::Drop { .. })) {
                    send(start, queued, work, f.index(), &now)?;
                }
            }
            let edge = |e: &EdgeId| b.edges[e.index()].target.index();
            if let Terminator::Branch { condition, yes, no } = &b.terminators[bi]
                && let Some(f) = self.flag_of.get(&condition.index())
                && now[self.stale_base + self.read_idx[&condition.index()]] == 0
            {
                if !check {
                    edge_buf.copy_from_slice(&now);
                    for &p in self.guards.get(f).map_or(&[][..], |v| v) {
                        edge_buf[p] &= T;
                        now[p] &= F;
                    }
                    self.transfer(&mut edge_buf, *yes);
                    self.transfer(&mut now, *no);
                    send(start, queued, work, edge(yes), &edge_buf)?;
                    send(start, queued, work, edge(no), &now)?;
                }
            } else {
                if let Terminator::Raise {
                    cause: RaiseCause::Fail(a),
                    ..
                } = b.terminators[bi]
                {
                    self.apply(m, &mut now, &Act::Use(a), check, &mut seen)
                        .map_err(|e| Error::Invalid(format!("b{bi}: {e}")))?;
                }
                if check && let Terminator::Return(r) = b.terminators[bi] {
                    for v in r.get(&b.pools.values).unwrap_or_default() {
                        for p in &self.n.vals[v.index()] {
                            need(p.root >= self.slots, || {
                                format!(
                                    "b{bi}: v{}: returns a pointer into local slot s{}",
                                    v.0, p.root
                                )
                            })
                            .map_err(Error::Invalid)?;
                        }
                    }
                    if self.has_dest && now[self.dest_escaped] != 0 {
                        return Err(Error::Invalid(format!(
                            "b{bi}: returns a view anchored on a local slot"
                        )));
                    }
                    if self.has_dest && self.dest_may_be_dropped(&now) {
                        return Err(Error::Invalid(format!(
                            "b{bi}: returns a dest view whose anchor may be dropped"
                        )));
                    }
                }
                if !check {
                    let edges: Vec<EdgeId> = match &b.terminators[bi] {
                        Terminator::Jump(e) => vec![*e],
                        Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                        Terminator::Switch {
                            arms, otherwise, ..
                        } => arms
                            .get(&b.pools.arms)
                            .unwrap_or_default()
                            .iter()
                            .map(|a| a.edge)
                            .chain([*otherwise])
                            .collect(),
                        _ => vec![],
                    };
                    if edges.is_empty() {
                        for t in successors_of_terminator(b, bi) {
                            send(start, queued, work, t, &now)?;
                        }
                    }
                    for e in edges {
                        edge_buf.copy_from_slice(&now);
                        self.transfer(&mut edge_buf, e);
                        send(start, queued, work, edge(&e), &edge_buf)?;
                    }
                }
            }
            Ok(())
        };
        while let Some(bi) = work.pop() {
            queued[bi] = false;
            step_block(&mut start, &mut queued, &mut work, false, bi)?;
        }
        for bi in 0..blocks {
            if start[bi].is_some() {
                step_block(&mut start, &mut queued, &mut work, true, bi)?;
            }
        }
        Ok(())
    }
    /// Drop place `q` through the joins `via`: in each of its pairings, only in the
    /// halves where one of them may be bound to it.
    pub(super) fn drop_bound(
        &self,
        now: &mut [u8],
        q: usize,
        via: &[usize],
        seen: &mut Vec<usize>,
    ) {
        let mut any = false;
        for &p in &self.pairs[q] {
            let d = self.bound_drop(now, q, p, via);
            any |= d != 0;
            now[p] |= d;
        }
        if any {
            self.invalidate(now, &self.lent[q], seen);
        }
    }
    /// What a drop through the joins `via` drops of place `q` in its pairing `p`: the
    /// halves where one of them may be bound to it. Applying it changes no half's
    /// presence, so it reads the same after [`Self::drop_bound`] as before.
    fn bound_drop(&self, now: &[u8], q: usize, p: usize, via: &[usize]) -> u8 {
        let bound = self.binds[q]
            .iter()
            .filter(|&&(t, pp, _)| pp == p && via.contains(&t))
            .fold(0, |m, &(_, _, bp)| m | now[bp]);
        dropped(now[p], true) & bound & D
    }
    /// Join place `t` may be place `q`, which a drop through the joins `via` dropped
    /// ([`Self::drop_bound`]): under a flag that guards both, `t` may be dropped only
    /// in the halves where `q` was; under any other, wherever it is.
    pub(super) fn drop_shadow(&self, now: &mut [u8], q: usize, t: usize, via: &[usize]) {
        for &pt in &self.pairs[t] {
            let same = self
                .guards
                .values()
                .find(|g| g.contains(&pt))
                .and_then(|g| self.pairs[q].iter().find(|p| g.contains(p)));
            let d = dropped(now[pt], true);
            now[pt] |= match same {
                Some(&p) => {
                    let (yes, no) = halves(self.bound_drop(now, q, p, via));
                    d & ((if yes { T } else { 0 }) | (if no { F } else { 0 }))
                }
                None => d,
            };
        }
    }
    /// Edge `e` binds each join parameter's places to what its argument names on
    /// this path: each tracked join place takes the state of the places it is now.
    pub(super) fn transfer(&self, now: &mut [u8], e: EdgeId) {
        let mut set = vec![];
        let mut bind = vec![];
        for (a, p) in ptr_args(self.b, &self.b.edges[e.index()]) {
            let Some(k) = self.joins.iter().position(|&j| j == p.index()) else {
                continue;
            };
            let srcs = match self.joins.iter().position(|&j| j == a.index()) {
                Some(i) => vec![Pl {
                    root: self.roots + i,
                    steps: vec![],
                    cut: false,
                }],
                None => self.n.vals[a.index()].clone(),
            };
            for (t, tp) in self.tracked.iter().enumerate() {
                if tp.root != self.roots + k {
                    continue;
                }
                let mut v = 0;
                for src in &srcs {
                    let mut x = src.clone();
                    x.steps.extend(tp.steps.iter().copied());
                    x.cut |= tp.cut;
                    for q in (0..self.tracked.len()).filter(|&q| overlap(&x, &self.tracked[q])) {
                        let z = self.pairs[q].iter().any(|&p| now[p] & Z != 0);
                        v |= if self.may_drop(now, q) { D } else { 0 } | if z { Z } else { 0 };
                    }
                }
                set.push((t, if v & D == 0 { Z } else { v | D }));
                // Bind the join to what its argument names here: a joined
                // argument's binding, or each place it names.
                for q in 0..self.tracked.len() {
                    for &(_, p, bp) in self.binds[q].iter().filter(|b| b.0 == t) {
                        let mut v = 0;
                        for src in &srcs {
                            let mut x = src.clone();
                            x.steps.extend(tp.steps.iter().copied());
                            x.cut |= tp.cut;
                            let joined = (x.root >= self.roots).then(|| {
                                let at = self.tracked.iter().position(|y| *y == x);
                                (at, &self.n.vals[self.joins[x.root - self.roots]])
                            });
                            let under = match joined {
                                None => overlap(&x, &self.tracked[q]),
                                Some((at, under)) => {
                                    if let Some(&(_, _, fp)) = at.and_then(|f| {
                                        self.binds[q].iter().find(|b| b.0 == f && b.1 == p)
                                    }) {
                                        v |= now[fp];
                                        continue;
                                    }
                                    under.iter().any(|u| {
                                        let mut y = u.clone();
                                        y.steps.extend(x.steps.iter().copied());
                                        overlap(&y, &self.tracked[q])
                                    })
                                }
                            };
                            v |= if under {
                                dropped(now[bp], true)
                            } else {
                                begun(now[bp])
                            };
                        }
                        bind.push((bp, if v == 0 { begun(now[bp]) } else { v }));
                    }
                }
            }
        }
        for (t, v) in set {
            for &p in &self.pairs[t] {
                now[p] = v;
            }
        }
        for (bp, v) in bind {
            now[bp] = v;
        }
    }
}
