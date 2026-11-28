use super::own::{Act, F, Own, T, Z};
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
        let mut acts = Vec::new();
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
                    self.apply(m, &mut now, a, check)
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
                    self.apply(m, &mut now, &Act::Use(a), check)
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
}
