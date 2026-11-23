//! Frame backing for the node of a local Log that never leaves its body. MEM-29's
//! node charge is logical (MEM-24) and MEM-32 binds only physical layout, so such a
//! node may sit in the frame: `log.new` admits its 24 bytes from the current limit
//! and is refused there exactly as on the task heap, and `log.finish` or the
//! binder's cleanup refunds them. The entries' storage stays on the task heap.
//!
//! The rule is intraprocedural and syntactic, and anything it does not recognise
//! escapes. `let x: Log[E] = log.new()` outside a closure, in a body with no
//! `bound bytes` scope, gets a frame node when every use of `x` is one of:
//! - a bare argument to a resolved function's view parameter (OWN-7), such as
//!   `log.append`, inside a closure or not;
//! - `view x`;
//! - `take x` as the argument of `log.finish`, outside a closure.
//!
//! Everything else (a move into a field, element, payload, capture or return, or
//! a `take` into any other call) escapes. Views cannot outlive the body that owns
//! `x`, so no view of the node outlives the frame. Without a `bound bytes` scope the
//! body has one byte limit: it pays `log.new`'s charge and takes the refund of
//! `log.finish` and of cleanup on every exit edge. A `bound steps` scope counts
//! steps and leaves that limit current.
use super::*;

/// The node's words: the entries' List carrier `{data, len, capacity}`.
const NODE_WORDS: u64 = 3;

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn is_core_call(&self, value: NodeId, path: &str) -> bool {
        self.kind(value) == NodeKind::Call
            && self
                .p
                .target(self.at, self.kids(value)[0])
                .and_then(|t| match t {
                    Target::Definition(id) => match self.p.by_definition[id.0 as usize] {
                        Some(Def::Fn(f)) => Some(f as usize),
                        _ => None,
                    },
                    _ => None,
                })
                .is_some_and(|f| self.p.fns[f].path.is_core(path))
    }

    /// The binders of this body's logs whose node never leaves it, by the rule above.
    fn frame_log_candidates(&self) -> HashSet<u32> {
        let tree = self.item.tree;
        let nodes = self.item.range.nodes.clone();
        let start = nodes.start;
        let mut parent = vec![u32::MAX; (nodes.end - start) as usize];
        let mut lets = Vec::new();
        for id in nodes {
            let n = NodeId(id);
            match tree.node(n).kind {
                NodeKind::Bound if self.token_text(n) != "steps" => return HashSet::default(),
                NodeKind::Let => lets.push(n),
                _ => {}
            }
            for k in tree.children(n) {
                if let Some(p) = parent.get_mut(k.0.wrapping_sub(start) as usize) {
                    *p = id;
                }
            }
        }
        let up = |n: NodeId| {
            parent
                .get(n.0.wrapping_sub(start) as usize)
                .filter(|&&p| p != u32::MAX)
                .map(|&p| NodeId(p))
        };
        let in_closure = |mut n: NodeId| {
            while let Some(p) = up(n) {
                if matches!(self.kind(p), NodeKind::Closure | NodeKind::ScopedClosure) {
                    return true;
                }
                n = p;
            }
            false
        };
        let mut found = HashSet::default();
        for s in lets {
            let kids = self.kids(s);
            if self.kind(kids[0]) == NodeKind::BindPattern
                && let Some(b) = self.binder(kids[0])
                && kids.len() > 1
                && self.is_core_call(kids[kids.len() - 1], "log.new")
                && !in_closure(s)
            {
                found.insert(b);
            }
        }
        if found.is_empty() {
            return found;
        }
        let stays = |u: NodeId| -> bool {
            let Some(p) = up(u) else { return false };
            let kids = self.kids(p);
            match self.kind(p) {
                NodeKind::Call if kids[0] != u => {
                    let Some(Target::Definition(id)) = self.call_target(kids[0]) else {
                        return false;
                    };
                    let Some(Def::Fn(f)) = self.p.by_definition[id.0 as usize] else {
                        return false;
                    };
                    let Some(arg) = kids[1..].iter().position(|&arg| arg == u) else {
                        return false;
                    };
                    self.p.fns[f as usize]
                        .params
                        .get(arg)
                        .is_some_and(|param| param.mode == Mode::View && !param.bytes)
                }
                NodeKind::Unary => match self.token_text(p) {
                    "view" => true,
                    "take" => up(p).is_some_and(|c| {
                        self.is_core_call(c, "log.finish")
                            && self.kids(c).get(1) == Some(&p)
                            && !in_closure(c)
                    }),
                    _ => false,
                },
                _ => false,
            }
        };
        let info = &self.p.items[self.at.module as usize][self.at.item as usize];
        for name in &info.resolution.names {
            if let Target::Binder(b) = name.target
                && found.contains(&b)
                && !(name.projections.start == name.projections.end && stays(name.node))
            {
                found.remove(&b);
            }
        }
        found
    }

    /// Whether the `let` statement `s` binds a log whose node is in the frame.
    pub(super) fn frame_log_let(&mut self, s: NodeId, pat: NodeId, value: NodeId) -> bool {
        if self.kind(s) != NodeKind::Let
            || self.kind(pat) != NodeKind::BindPattern
            || !self.is_core_call(value, "log.new")
        {
            return false;
        }
        let Some(b) = self.binder(pat) else {
            return false;
        };
        if self.frame_log_binders.is_none() {
            self.frame_log_binders = Some(self.frame_log_candidates());
        }
        self.frame_log_binders
            .as_ref()
            .is_some_and(|found| found.contains(&b))
    }

    /// The frame bytes of a log node, for `log.new`'s allocation.
    pub(super) fn frame_log_bytes(&mut self, n: NodeId) -> Option<ValueId> {
        let words = self.t.mk(TyKind::Array(scalar(Scalar::U64), NODE_WORDS));
        let row = self.place_row(words, "frame log node without a place", n)?;
        let bytes = self.entry_slot(PlaceTy::Declared(row), n);
        Some(self.pointer(bytes, n))
    }

    /// Clean the frame log owned at `address`, with entries of type `item`: drop
    /// its entries, then refund the node's charge; the node stays in the frame.
    pub(super) fn drop_frame_log(
        &mut self,
        address: AddressId,
        item: TyId,
        n: NodeId,
    ) -> Option<()> {
        let node_at = self.address(
            AddressBase::Address(address),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let node = self.read_bits(node_at, Ty::Ptr, n);
        let list = self.list_ty(item, n)?;
        let list_id = self.runtime_type(list, n)?;
        self.emit_drop(AddressBase::Value(node), list_id, n);
        let (charge, _) = self.owner_charge(list, None, n)?;
        self.inst(Inst::FrameRefund { charge }, n);
        Some(())
    }
}
