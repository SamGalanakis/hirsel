//! Where slots are needed, and which slots may share bytes for it.

use super::{Loans, Point, escaping, provenance, slots_with, targets};
use crate::*;

pub(super) fn preserve_guards(b: &Body, from: &[Vec<u32>], keep: &mut [bool]) {
    // A true drop flag proves its owner's initialization. Sharing that flag
    // with an unrelated boolean loses the implication in the merged IR.
    let conditions: std::collections::HashSet<_> = b
        .terminators
        .iter()
        .filter_map(|t| match t {
            Terminator::Branch { condition, .. } => Some(*condition),
            _ => None,
        })
        .collect();
    for inst in &b.insts {
        if let Inst::Read {
            result, address, ..
        } = inst
            && conditions.contains(result)
        {
            for &slot in &from[b.values.len() + address.index()] {
                if b.slots[slot as usize]
                    == PlaceTy::Value(Ty::Scalar(hirsel_core_layout::Scalar::Bool))
                {
                    keep[slot as usize] = true;
                }
            }
        }
    }
}

/// Each candidate slot's needed ranges, as [`share_slots`] defines them:
/// (block, from, to), inclusive, the block's length being its terminator; in
/// block order, at most one a block.
pub(super) fn needed_ranges(
    b: &Body,
    from: &[Vec<u32>],
    candidates: &[usize],
) -> Vec<Vec<(u32, u32, u32)>> {
    let n = candidates.len();
    let slots = slots_with(b, from);
    let nb = b.blocks.len();
    let words = n.div_ceil(64);
    let lens: Vec<u32> = b.blocks.iter().map(|blk| blk.insts.len).collect();
    // Each candidate's accesses by block: (block, first, last) index.
    let mut acc = vec![0u64; nb * words];
    let mut spans: Vec<Vec<(u32, u32, u32)>> = vec![vec![]; n];
    for (c, &s) in candidates.iter().enumerate() {
        let mut points: Vec<Point> = slots.lives[s]
            .iter()
            .chain(&slots.deads[s])
            .chain(&slots.uses[s])
            .copied()
            .collect();
        points.sort_unstable();
        for (bi, j) in points {
            acc[bi as usize * words + c / 64] |= 1 << (c % 64);
            match spans[c].last_mut() {
                Some(last) if last.0 == bi => last.2 = j,
                _ => spans[c].push((bi, j, j)),
            }
        }
    }
    let mut succ: Vec<Vec<usize>> = vec![vec![]; nb];
    for (bi, out) in succ.iter_mut().enumerate() {
        let code = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        out.extend(code.iter().filter_map(|i| i.fail(b)).map(|f| f.index()));
        out.extend(targets(b, bi));
        out.sort_unstable();
        out.dedup();
    }
    // Accessed on some path before a block's entry, and after its exit.
    let mut before = vec![0u64; nb * words];
    let mut after = vec![0u64; nb * words];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in 0..nb {
            for &t in &succ[bi] {
                for w in 0..words {
                    let into = before[bi * words + w] | acc[bi * words + w];
                    if into & !before[t * words + w] != 0 {
                        before[t * words + w] |= into;
                        changed = true;
                    }
                }
            }
        }
        for bi in (0..nb).rev() {
            for &t in &succ[bi] {
                for w in 0..words {
                    let into = after[t * words + w] | acc[t * words + w];
                    if into & !after[bi * words + w] != 0 {
                        after[bi * words + w] |= into;
                        changed = true;
                    }
                }
            }
        }
    }
    // Each candidate's needed ranges: (block, from, to), inclusive, the block's
    // length being its terminator; in block order, at most one a block.
    let mut ranges: Vec<Vec<(u32, u32, u32)>> = vec![vec![]; n];
    for bi in 0..nb {
        for w in 0..words {
            let (pre, post, here) = (
                before[bi * words + w],
                after[bi * words + w],
                acc[bi * words + w],
            );
            let mut live = (pre & post) | here;
            while live != 0 {
                let c = w * 64 + live.trailing_zeros() as usize;
                live &= live - 1;
                let bit = 1u64 << (c % 64);
                let span = (here & bit != 0).then(|| {
                    let i = spans[c].partition_point(|&(sb, _, _)| (sb as usize) < bi);
                    (spans[c][i].1, spans[c][i].2)
                });
                let lo = if pre & bit != 0 {
                    0
                } else {
                    span.map_or(0, |x| x.0)
                };
                let hi = if post & bit != 0 {
                    lens[bi]
                } else {
                    span.map_or(lens[bi], |x| x.1)
                };
                ranges[c].push((bi as u32, lo, hi));
            }
        }
    }
    ranges
}

/// Whether two slots' [`needed_ranges`] meet at some point.
fn ranges_meet(x: &[(u32, u32, u32)], y: &[(u32, u32, u32)]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        let ((xb, xf, xt), (yb, yf, yt)) = (x[i], y[j]);
        if xb < yb {
            i += 1;
        } else if yb < xb {
            j += 1;
        } else {
            if xf <= yt && yf <= xt {
                return true;
            }
            i += 1;
            j += 1;
        }
    }
    false
}

/// Slots lent `view` to a call whose results include a pointer, or that has a
/// destination: the result may be anchored to the lent place (OWN-20), and
/// provenance does not follow call results, so the slot's uses do not show
/// every access of it.
pub(super) fn anchored_views(b: &Body, signatures: &[Signature], from: &[Vec<u32>]) -> Vec<bool> {
    let mut out = vec![false; b.slots.len()];
    for site in &b.calls {
        let Some(g) = signatures.get(site.signature.index()) else {
            continue;
        };
        if g.dest.is_none() && !g.results.contains(&Ty::Ptr) {
            continue;
        }
        let args = site.args.get(&b.pools.args).unwrap_or_default();
        for (arg, param) in args[usize::from(g.dest.is_some())..].iter().zip(&g.params) {
            if let (
                Arg::Address(x),
                Param::Address {
                    loan: Loan::View, ..
                },
            ) = (*arg, param)
            {
                for &s in &from[b.values.len() + x.index()] {
                    out[s as usize] = true;
                }
            }
        }
    }
    out
}

/// Which of `slots` conflict: `conflicts[i][j]` when slots `slots[i]` and
/// `slots[j]` may both be needed at one point, so they cannot share bytes. A
/// slot is needed where [`share_slots`] says, and every access of it is one
/// of its uses when its address leaves the body only as a loan lasting the
/// call or a channel receive's destination ([`Loans::Scoped`]) and it is not
/// lent `view` to a call that may anchor its result to it; any other slot
/// conflicts with every slot.
pub(crate) fn slot_conflicts(
    b: &Body,
    signatures: &[Signature],
    slots: &[usize],
) -> Vec<Vec<bool>> {
    let from = provenance(b);
    let escapes = escaping(b, signatures, &from, Loans::Scoped);
    let anchored = anchored_views(b, signatures, &from);
    let ranges = needed_ranges(b, &from, slots);
    let open: Vec<bool> = slots.iter().map(|&s| !escapes[s] && !anchored[s]).collect();
    let n = slots.len();
    let mut out = vec![vec![false; n]; n];
    for i in 0..n {
        for j in i + 1..n {
            let meet = !open[i] || !open[j] || ranges_meet(&ranges[i], &ranges[j]);
            out[i][j] = meet;
            out[j][i] = meet;
        }
    }
    out
}
