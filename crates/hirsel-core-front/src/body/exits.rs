//! Loop and labelled-block frames and the exits that leave them: `break`, whose
//! value is its frame's value (LANG-6, OWN-10), and `continue`.
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum FrameKind {
    Loop,
    While { condition: Option<NodeId> },
    Block,
}
pub(super) struct Frame {
    pub(super) label: Option<String>,
    pub(super) kind: FrameKind,
    /// The loop header `continue` jumps to.
    pub(super) header: Option<BlockId>,
    pub(super) exit: usize,
    /// The value type its exits carry, once one fixes it.
    pub(super) ty: Option<TyId>,
    /// Expected payload constraints survive until the first exit fixes its type.
    pub(super) hint: Option<Want>,
    /// Whether any exit names it, reachable or not.
    pub(super) left: bool,
    /// The undo log's length and the declaration count at its start.
    pub(super) mark: usize,
    pub(super) born: u32,
    /// Where its exit edges begin in `exits`.
    pub(super) exits: usize,
    pub(super) scopes: usize,
    /// `scope_slots`' length at its start: the slots declared inside it follow.
    pub(super) slots: usize,
    /// The owners begun before it.
    pub(super) owners: usize,
    /// The temporaries its `break` values reach, cleaned by the statement that
    /// encloses it (LANG-6), each behind its own drop flag.
    pub(super) carried: Vec<cleanup::Owner>,
    /// The block a `break` value leaves, for a loop or labelled block: the cleanup
    /// at its closing `}` drops the locals declared in the frame (OWN-10).
    pub(super) body: Option<NodeId>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// The want of a join's next arm, `if`, `if let`, `match` or a frame's `break`,
    /// given `first`, the type of the first arm that did not diverge. A Never arm
    /// constrains nothing (LANG-11); a view want (OWN-7) binds to the first arm, so a
    /// later arm must match it exactly and a join of views owns nothing (OWN-16).
    pub(super) fn arm_want(&self, want: Option<Want>, first: Option<TyId>) -> Option<Want> {
        want.filter(|w| self.fixed_want(*w) && !(w.view && first.is_some()))
            .or(first.and_then(expect))
            .or(want)
    }
    pub(super) fn push_frame(
        &mut self,
        label: Option<String>,
        kind: FrameKind,
        hint: Option<Want>,
    ) {
        let exit = self.new_join();
        // A view want is fixed by the first `break` instead (OWN-7, OWN-16).
        let ty = hint
            .filter(|w| self.fixed_want(*w) && !w.view)
            .map(|w| w.ty);
        self.frames.push(Frame {
            label,
            kind,
            header: None,
            exit,
            ty,
            hint,
            left: false,
            mark: self.log.len(),
            born: self.born,
            exits: self.exits.len(),
            scopes: self.pattern_scope_mark(),
            slots: self.scope_slots.len(),
            owners: self.owner_mark(),
            carried: vec![],
            body: None,
        });
    }
    /// Pop the innermost frame, joining its exits (OWN-16).
    pub(super) fn pop_frame(&mut self, n: NodeId) -> Frame {
        let fi = self.frames.len() - 1;
        self.merge_exits(fi, n);
        self.frames.pop().unwrap()
    }
    /// Continue at frame `f`'s exit: the temporaries its `break` values reach now
    /// belong to the enclosing statement (LANG-6).
    pub(super) fn land_frame(&mut self, f: &mut Frame) -> Option<ValueId> {
        let v = self.land(f.exit);
        if self.live() && !f.carried.is_empty() {
            self.owners.append(&mut f.carried);
            self.epoch += 1;
        }
        v
    }

    /// The frame an exit leaves: the labelled one, or the innermost loop.
    pub(super) fn frame_of(&mut self, n: NodeId) -> Option<usize> {
        let label = self.item.tree.node(n).token.map(|_| self.token_text(n));
        let i = self.frames.iter().rposition(|f| match label {
            Some(l) => f.label.as_deref() == Some(l),
            None => f.kind != FrameKind::Block,
        });
        if i.is_none() {
            // Refused LANG-15 by the resolver.
            self.halt();
        }
        i
    }

    pub(super) fn break_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let value = self.kids(n).first().copied();
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if matches!(self.frames[i].kind, FrameKind::While { .. }) && value.is_some() {
            // Refused LANG-15 by the resolver.
            self.halt();
            return (NEVER, None);
        }
        let fixed = self.frames[i].ty;
        let (t, v) = match value {
            Some(e) => self.expr(e, self.arm_want(self.frames[i].hint, fixed)),
            None => {
                if let Some(f) = fixed {
                    self.coerce(
                        UNIT,
                        Want {
                            ty: f,
                            view: false,
                            payload: 0,
                        },
                        n,
                    );
                }
                (UNIT, None)
            }
        };
        // MEM-7: a carried value whose type names a region opened inside the frame
        // leaves that region's block.
        if let Some(e) = value
            && self.opened_region_in(t, self.frames[i].slots).is_some()
        {
            self.refuse(
                "MEM-7",
                e,
                "a value whose type names a region opened in the loop cannot leave it",
            );
        }
        let f = &mut self.frames[i];
        f.left = true;
        if f.ty.is_none() && t != NEVER {
            f.ty = Some(t);
        }
        let (exit, ty) = (f.exit, f.ty.unwrap_or(t));
        // The carried value goes to the exit's join; the rest is cleaned (FAIL-8, 9).
        self.consume(v, n);
        if let (Some(v), Ok(Rep::Place(id))) = (v, self.rep(ty))
            && !self.is_view(ty)
            && self.e.runs_code(id)
        {
            self.pending = Some((AddressBase::Value(v), id));
        }
        self.break_owners(i, value, t, n);
        self.goto(exit, ty, v, n);
        self.exit_edge(i);
        self.dead = true;
        (NEVER, None)
    }

    /// A `break` value is its frame's value, as a block's tail is the block's
    /// (LANG-6, OWN-10): a local declared in the frame that it reaches is refused,
    /// and the temporaries it reaches are not cleaned on the edge but carried to the
    /// frame's exit, to end with the statement that encloses the frame.
    fn break_owners(&mut self, i: usize, value: Option<NodeId>, t: TyId, n: NodeId) {
        let (owners, scopes) = (self.frames[i].owners, self.frames[i].scopes);
        let fact = self.anchors.last;
        let reach = match value {
            Some(_) if fact != 0 && t != NEVER && self.type_kind(t) == Kind::Scoped => fact,
            _ => 0,
        };
        if reach == 0 {
            self.unwind(owners, scopes, n);
            return;
        }
        if let Some(e) = value
            && let Some(s) = self.local_place_anchor(reach, self.frames[i].slots)
        {
            let name = self.slot_name(s);
            let cause = Cause {
                site: self.cause(e),
                role: CausalRole::ViewedHere,
            };
            let site = self.frames[i]
                .body
                .map_or(self.span(n), |b| self.close_brace(b));
            self.refuse_from("OWN-10", site, vec![cause], format!("`{name}` is viewed"));
        }
        let start = self.retained_owners.len();
        self.retain_anchor_owners(reach);
        let mut kept = start;
        for k in start..self.retained_owners.len() {
            let o = self.retained_owners[k];
            if o >= owners && !self.retained_owners[start..kept].contains(&o) {
                self.retained_owners[kept] = o;
                kept += 1;
            }
        }
        self.retained_owners.truncate(kept);
        for k in start..kept {
            let o = self.retained_owners[k];
            let cleanup::Owner::Temp(t) = self.owners[o] else {
                continue;
            };
            // Every break of this frame that reaches the temporary shares one
            // carried owner, so the exit holds and cleans it once.
            let shared = self.frames[i].carried.iter().find_map(|c| match c {
                cleanup::Owner::Temp(c) if c.value == t.value => Some(*c),
                _ => None,
            });
            match shared {
                Some(c) => self.carry_into(o, c, n),
                None => {
                    if let Some(carried) = self.carry_temporary(o, n) {
                        self.frames[i].carried.push(carried);
                    }
                }
            }
        }
        let retained = PoolRange::new(start as u32, (kept - start) as u32);
        self.unwind_keeping(owners, scopes, retained, n);
        self.retained_owners.truncate(start);
    }

    pub(super) fn continue_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if self.frames[i].kind == FrameKind::Block {
            // Refused LANG-15 by the resolver.
            self.halt();
            return (NEVER, None);
        }
        self.unwind(self.frames[i].owners, self.frames[i].scopes, n);
        self.source_step(n);
        if let Some(h) = self.frames[i].header {
            self.jump(h, &[], n);
        }
        self.cur = None;
        self.back_edge(i, self.span(n));
        self.dead = true;
        (NEVER, None)
    }
}
