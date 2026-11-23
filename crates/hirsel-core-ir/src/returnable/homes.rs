//! Which slots a variant keeps in its record, and where.

use super::*;

/// The slots a variant keeps in its record: each one whose bytes or address
/// may be needed after one of its sites. A slot is needed at a site when
/// - its address leaves what provenance follows (`SlotFacts::escapes`);
/// - the site's own call uses it, since the callee is given the address again
///   at every resume;
/// - a value that may point into it is live after the site, since a pointer
///   kept across the wait must still reach the same bytes; or
/// - a use of it is reachable from the site, on a terminator or failure edge,
///   with no `StorageLive` or `StorageDead` of it between. Neither ends a need
///   that a marker does not reach: a `StorageLive` begins the slot unwritten,
///   and no use follows a `StorageDead` without one (crate docs, "Values, slots
///   and addresses").
///
/// Every other slot stays a frame slot of the variant, with its markers, so
/// the engine can still promote or share it.
pub(super) fn homed_slots(
    b: &Body,
    signatures: &[Signature],
    sites: &BTreeSet<usize>,
    live: &BTreeMap<usize, Set>,
) -> Vec<bool> {
    let facts = crate::extent::slot_facts(b, signatures);
    let k = b.slots.len();
    let words = k.div_ceil(64).max(1);
    let mut used: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (s, points) in facts.uses.iter().enumerate() {
        for &p in points {
            used.entry(p).or_default().push(s);
        }
    }
    let gen_at = |now: &mut Set, bi: usize, j: usize| {
        for &s in used.get(&(bi as u32, j as u32)).into_iter().flatten() {
            put(now, s);
        }
    };
    let n = b.blocks.len();
    let mut live_in: Vec<Set> = vec![vec![0; words]; n];
    let mut homed = facts.escapes.clone();
    let pass = |live_in: &mut Vec<Set>, homed: &mut Vec<bool>, record: bool| -> bool {
        let mut changed = false;
        for bi in (0..n).rev() {
            let mut now = vec![0; words];
            for t in successors(b, bi) {
                join(&mut now, &live_in[t]);
            }
            let range = b.blocks[bi].insts;
            let first = range.start as usize;
            let insts = range.get(&b.insts).unwrap_or_default();
            gen_at(&mut now, bi, insts.len());
            for (j, inst) in insts.iter().enumerate().rev() {
                match *inst {
                    Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                        cut(&mut now, slot.index())
                    }
                    _ => gen_at(&mut now, bi, j),
                }
                if let Some(f) = inst.fail(b) {
                    join(&mut now, &live_in[f.index()]);
                }
                if record && sites.contains(&(first + j)) {
                    // `now` holds what the site uses, what follows it, and what
                    // its failure continues with.
                    for (s, h) in homed.iter_mut().enumerate() {
                        *h |= has(&now, s);
                    }
                }
            }
            if now != live_in[bi] {
                live_in[bi] = now;
                changed = true;
            }
        }
        changed
    };
    while pass(&mut live_in, &mut homed, false) {}
    pass(&mut live_in, &mut homed, true);
    let nv = b.values.len();
    for set in live.values() {
        for v in (0..nv).filter(|&v| has(set, v)) {
            for &s in &facts.from[v] {
                homed[s as usize] = true;
            }
        }
    }
    homed
}

/// Places `slots` (slot, size, align) at or after `base`, so two slots share
/// bytes only when no point needs both ([`crate::extent::slot_conflicts`]) (rustc's generator
/// layout). First fit, largest first, ties in slot order: the same body
/// always gives the same layout. Returns (slot, offset, end) per slot.
pub(super) fn place(
    b: &Body,
    signatures: &[Signature],
    base: u64,
    slots: &[(usize, u64, u64)],
) -> Vec<(usize, u64, u64)> {
    let ids: Vec<usize> = slots.iter().map(|p| p.0).collect();
    let conflicts = crate::extent::slot_conflicts(b, signatures, &ids);
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(slots[i].1), i));
    let mut placed: Vec<(usize, u64, u64)> = vec![];
    for i in order {
        let (_, size, align) = slots[i];
        let mut off = round(base, align);
        // Move past the conflicting slot that overlaps furthest, until none does.
        while let Some(end) = placed
            .iter()
            .filter(|&&(j, o, e)| conflicts[i][j] && off < e && o < off + size.max(1))
            .map(|p| p.2)
            .max()
        {
            off = round(end, align);
        }
        placed.push((i, off, off + size));
    }
    placed
        .into_iter()
        .map(|(i, off, end)| (slots[i].0, off, end))
        .collect()
}
