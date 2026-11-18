//! Frame backing for a local List that never escapes its body. MEM-32 binds only
//! physical layout and MEM-24 charges are logical, so such a list's storage may sit in
//! the frame: every charge, refund and refusal happens at the operation and with the
//! value it has on the task heap.
//!
//! The rule is intraprocedural and syntactic, and anything it does not recognise
//! escapes. `let mut x = list.new()` in a body without a `bound` scope, outside a
//! closure, gets a frame slot when every use of `x` is one of:
//! - `mut x` as the first argument of a core `list` operation the front expands in
//!   place (`push`, `pop`, `set`, `swap`, `clear`, `reserve`);
//! - `mut x` as the first argument of `list.append` outside a loop. Bulk
//!   appends inside a loop and `fill` can move straight to the heap. Keep
//!   their frame-growth arms out of the loops that use the resulting carrier.
//!   A list initialized before a dynamic loop that both pushes and pops it
//!   also starts on the heap. That loop reuses its capacity after each pop,
//!   so the inline frame growth would save only the initial allocations.
//! - the base of an index, `x[i]`;
//! - a bare argument to a resolved function's view parameter (OWN-7);
//! - a `mut` argument of a call, which is lent for the call (OWN-7);
//! - the source of a viewing `for`, or `view x`;
//!
//! and the use is not inside a closure. Everything else (a move, `take x`, a return,
//! a field, element, payload or capture, or a direct assignment to `x`) escapes.
//! Its element type must be plain so dropping the list visits no element
//! (MEM-32), and fixed so the slot's element count is known here.
//!
//! The slot holds [`FRAME_LIST_BYTES`]. Local-only growth stays in the frame
//! while it fits. A slot whose storage may be lent records its original payer
//! and spills through the ordinary grow operation. Move-out promotes it with
//! its existing logical charge. Dropping the
//! list refunds its charge and frees only heap storage. The current limit is the one
//! limit of the body: no `bound` scope changes it, so it pays each frame charge and
//! takes each refund.
use super::*;

use hirsel_core_layout::{FRAME_LIST_BYTES, FRAME_LIST_LINK_BYTES};

/// The core `list` operations that may take a frame-backed list as `mut`.
const IN_PLACE: [&str; 6] = [
    "list.push",
    "list.pop",
    "list.set",
    "list.swap",
    "list.clear",
    "list.reserve",
];

/// Storage reserved for the frame-backed path of a list.
#[derive(Clone, Copy)]
pub(super) struct FrameList {
    pub bytes: AddressId,
    pub item: TyId,
    pub lent: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn core_fn_at(&self, callee: NodeId) -> Option<usize> {
        match self.p.target(self.at, callee)? {
            Target::Definition(id) => match self.p.by_definition[id.0 as usize] {
                Some(Def::Fn(f)) => Some(f as usize),
                _ => None,
            },
            _ => None,
        }
    }
    fn is_list_new(&self, value: NodeId) -> bool {
        self.kind(value) == NodeKind::Call
            && self.kids(value).len() == 1
            && self
                .core_fn_at(self.kids(value)[0])
                .is_some_and(|f| self.p.fns[f].path.is_core("list.new"))
    }
    /// The binders of this body's lists that never escape, by the rule above.
    fn frame_candidates(&self) -> HashMap<u32, bool> {
        let tree = self.item.tree;
        let nodes = self.item.range.nodes.clone();
        let start = nodes.start;
        let mut parent = vec![u32::MAX; (nodes.end - start) as usize];
        let mut lets = Vec::new();
        for id in nodes {
            let n = NodeId(id);
            match tree.node(n).kind {
                NodeKind::Bound => return HashMap::default(),
                NodeKind::LetMut => lets.push(n),
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
        let enclosing = |mut n: NodeId, kinds: &[NodeKind]| {
            while let Some(p) = up(n) {
                if kinds.contains(&self.kind(p)) {
                    return Some(p);
                }
                n = p;
            }
            None
        };
        let in_closure = |n| enclosing(n, &[NodeKind::Closure, NodeKind::ScopedClosure]).is_some();
        let enclosing_loop = |n| {
            enclosing(
                n,
                &[
                    NodeKind::Loop,
                    NodeKind::While,
                    NodeKind::For,
                    NodeKind::ForTake,
                ],
            )
        };
        let dynamic_loop = |n| {
            if self.kind(n) != NodeKind::For {
                return true;
            }
            let mut source = self.kids(n)[1];
            while self.kind(source) == NodeKind::Group {
                source = self.kids(source)[0];
            }
            !(self.kind(source) == NodeKind::Binary
                && matches!(self.token_text(source), ".." | "..=")
                && self
                    .kids(source)
                    .iter()
                    .all(|&n| self.token_kind(n) == Some(TokenKind::Integer)))
        };
        let mut found = HashMap::default();
        for s in lets {
            let kids = self.kids(s);
            if self.kind(kids[0]) == NodeKind::BindPattern
                && let Some(b) = self.binder(kids[0])
                && kids.len() > 1
                && self.is_list_new(kids[kids.len() - 1])
                && !in_closure(s)
            {
                found.insert(b, false);
            }
        }
        if found.is_empty() {
            return found;
        }
        let info = &self.p.items[self.at.module as usize][self.at.item as usize];
        let mut stack_ops = HashMap::default();
        // Some(true) means the callee needs to identify this allocation.
        let mut backing = |b: u32, u: NodeId| {
            let p = up(u)?;
            let kids = self.kids(p);
            match self.kind(p) {
                NodeKind::Index if kids[0] == u => Some(false),
                NodeKind::Call if kids[0] != u => {
                    let Target::Definition(id) = self.call_target(kids[0])? else {
                        return None;
                    };
                    let Some(Def::Fn(f)) = self.p.by_definition[id.0 as usize] else {
                        return None;
                    };
                    let arg = kids[1..].iter().position(|&arg| arg == u)?;
                    let param = self.p.fns[f as usize].params.get(arg)?;
                    (param.mode == Mode::View && !param.bytes).then_some(false)
                }
                NodeKind::For if kids[1] == u => Some(false),
                NodeKind::Unary => match self.token_text(p) {
                    "view" => Some(false),
                    "mut" => {
                        let c = up(p)?;
                        if self.kind(c) != NodeKind::Call {
                            return None;
                        }
                        let args = self.kids(c);
                        if args.get(1) == Some(&p)
                            && let Some(f) = self.core_fn_at(args[0])
                        {
                            let callee = &self.p.fns[f];
                            let path = &callee.path;
                            if callee.body.is_none() {
                                if path.is_core("list.fill")
                                    || (path.is_core("list.append") && enclosing_loop(c).is_some())
                                {
                                    return None;
                                }
                                let push = path.is_core("list.push");
                                if (push || path.is_core("list.pop"))
                                    && let Some(loop_node) = enclosing_loop(c)
                                    && dynamic_loop(loop_node)
                                    && info.resolution.binders[b as usize].site.span.start
                                        < tree.node(loop_node).origin.span.start
                                    && stack_ops
                                        .insert((b, loop_node), push)
                                        .is_some_and(|earlier| earlier != push)
                                {
                                    return None;
                                }
                                if IN_PLACE.iter().any(|op| path.is_core(op))
                                    || path.is_core("list.append")
                                    || path.is_core("list.put_word_le")
                                {
                                    return Some(false);
                                }
                            }
                        }
                        (args[0] != p).then_some(true)
                    }
                    _ => None,
                },
                _ => None,
            }
        };
        for name in &info.resolution.names {
            if let Target::Binder(b) = name.target
                && let Some(lent) = found.get_mut(&b)
            {
                if name.projections.start == name.projections.end
                    && !in_closure(name.node)
                    && let Some(loaned) = backing(b, name.node)
                {
                    *lent |= loaned;
                } else {
                    found.remove(&b);
                }
            }
        }
        found
    }

    /// Whether the `let mut` binder `b`, initialised by `value`, keeps its first
    /// storage in the frame.
    pub(super) fn frame_binder(&mut self, b: u32, value: NodeId) -> Option<bool> {
        if !self.is_list_new(value) {
            return None;
        }
        if self.frame_binders.is_none() {
            self.frame_binders = Some(self.frame_candidates());
        }
        self.frame_binders.as_ref()?.get(&b).copied()
    }

    /// Give the list slot at `address`, of type `ty`, its frame bytes when its
    /// element is plain and fixed and at least the first growth's four fit.
    pub(super) fn begin_frame_list(&mut self, ty: TyId, address: AddressId, lent: bool, n: NodeId) {
        let TyKind::Adt(d, args) = self.t.kind(ty) else {
            return;
        };
        if self.p.decls[d.index()].role != Some(Role::List) {
            return;
        }
        let item = args[0];
        if self.list_storage_static(item) != Some(true) || self.drop_id(item).is_some() {
            return;
        }
        let AddressBase::Slot(slot) = self.b.addresses[address.index()].base else {
            return;
        };
        let size = FRAME_LIST_BYTES + if lent { FRAME_LIST_LINK_BYTES } else { 0 };
        let words = self.t.mk(TyKind::Array(scalar(Scalar::U64), size / 8));
        let Some(row) = self.place_row(words, "frame list bytes without a place", n) else {
            return;
        };
        let bytes = self.entry_slot(PlaceTy::Declared(row), n);
        self.frame_slots
            .insert(slot, FrameList { bytes, item, lent });
    }

    /// The frame list `header` points at, when it is one of this body's.
    pub(super) fn frame_list(&self, header: ValueId) -> Option<FrameList> {
        if self.frame_slots.is_empty() {
            return None;
        }
        let chain = self.place_chain(header);
        match chain.root {
            AddressBase::Slot(s) if chain.steps.is_empty() => self.frame_slots.get(&s).copied(),
            _ => None,
        }
    }

    /// The frame list whose slot is `address`.
    pub(super) fn frame_list_at(&self, address: AddressId) -> Option<FrameList> {
        match self.b.addresses[address.index()].base {
            AddressBase::Slot(s) if self.b.addresses[address.index()].steps.len == 0 => {
                self.frame_slots.get(&s).copied()
            }
            _ => None,
        }
    }

    /// MEM-24's charge of `cap` elements of logical size `logical`, for a capacity
    /// whose charge was admitted, so it cannot overflow.
    pub(super) fn admitted_list_charge(
        &mut self,
        cap: ValueId,
        logical: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (charge, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, logical, n);
        let seven = self.konst(Ty::Scalar(Scalar::U64), 7, n)?;
        let (rounded, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, charge, seven, n);
        let mask = self.konst(Ty::Scalar(Scalar::U64), !7, n)?;
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::BitAnd,
                left: rounded,
                right: mask,
                fail: None,
            },
            n,
        );
        Some(result)
    }

    /// Drop the frame list at `address`: storage in its frame bytes is refunded and
    /// stays; heap storage is dropped as any list's.
    pub(super) fn drop_frame_list(
        &mut self,
        f: FrameList,
        address: AddressId,
        id: TypeId,
        n: NodeId,
    ) -> Option<()> {
        if f.lent {
            self.emit_drop(AddressBase::Address(address), id, n);
            return Some(());
        }
        let carrier = self.list_carrier(f.item, n)?;
        let header = self.pointer(address, n);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let frame = self.pointer(f.bytes, n);
        let framed = self.list_compare(CompareOp::Eq, data, frame, n);
        let (refund, heap, done) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.list_branch(framed, refund, heap, n);
        self.start(refund);
        let cap = self.carrier_word(header, carrier, SeqWord::Cap, n);
        let logical = self.layout_size(f.item, true, n)?;
        let charge = self.admitted_list_charge(cap, logical, n)?;
        self.inst(Inst::FrameRefund { charge }, n);
        self.jump(done, &[], n);
        self.start(heap);
        self.emit_drop(AddressBase::Address(address), id, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }
    /// Growth of the frame list `header` points at, entered where `grow` would
    /// choose between replacing owned storage (`like`) and allocating (`current`).
    /// A new capacity that fits the frame bytes is admitted there; one past them
    /// takes heap storage, copies, and refunds the frame charge after the install,
    /// as `grow` admits the whole new charge before refunding the old.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn frame_growth(
        &mut self,
        f: FrameList,
        header: ValueId,
        carrier: TypeId,
        plan: &super::lists::ListPlan,
        [old_cap, new_cap, len, moved]: [ValueId; 4],
        owned: ValueId,
        [like, current]: [BlockId; 2],
        success: BlockId,
        error: BlockId,
        n: NodeId,
    ) -> Option<()> {
        let frame = self.pointer(f.bytes, n);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let field = self.edge_field(f.item, n)?;
        let room = self.layout_word(LayoutQuery::FrameCapacity(field), n);
        let fits = self.list_compare(CompareOp::Le, new_cap, room, n);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let [unowned, first] = [(); 2].map(|_| self.new_block(&[]));
        let inside = if f.lent {
            None
        } else {
            let [inside, heap] = [(); 2].map(|_| self.new_block(&[]));
            let framed = self.list_compare(CompareOp::Eq, data, frame, n);
            self.list_branch(framed, inside, heap, n);
            self.start(heap);
            Some(inside)
        };
        self.list_branch(owned, like, unowned, n);
        self.start(unowned);
        self.list_branch(fits, first, current, n);
        // The first storage, or static bytes' copy, in the frame.
        self.start(first);
        let place = if f.lent {
            AllocPlace::LentFrame(frame)
        } else {
            AllocPlace::Frame(frame)
        };
        let (block, available) = self.list_allocate(plan, place, n);
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let (copy, refused) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(ok, copy, refused, n);
        self.start(refused);
        self.jump(error, &[available], n);
        self.start(copy);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let (fill, commit) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(nonempty, fill, commit, n);
        self.start(fill);
        self.frame_copy(frame, data, moved, n);
        self.jump(commit, &[], n);
        self.start(commit);
        self.set_carrier_word(header, carrier, SeqWord::Data, frame, n);
        self.set_carrier_word(header, carrier, SeqWord::Cap, new_cap, n);
        self.jump(success, &[], n);
        // Local-only storage keeps its in-frame growth. The shared grow path
        // replaces lent storage and settles its original payer.
        let Some(inside) = inside else {
            return Some(());
        };
        let [regrow, spill] = [(); 2].map(|_| self.new_block(&[]));
        self.start(inside);
        let old_charge = self.admitted_list_charge(old_cap, plan.logical, n)?;
        self.list_branch(fits, regrow, spill, n);
        self.start(regrow);
        let (block, available) = self.list_allocate(plan, AllocPlace::Frame(frame), n);
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let (grown, refused) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(ok, grown, refused, n);
        self.start(refused);
        self.jump(error, &[available], n);
        self.start(grown);
        self.set_carrier_word(header, carrier, SeqWord::Cap, new_cap, n);
        self.inst(Inst::FrameRefund { charge: old_charge }, n);
        self.jump(success, &[], n);
        self.start(spill);
        let (block, available) = self.list_allocate(plan, AllocPlace::Current, n);
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let (moved_out, refused) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(ok, moved_out, refused, n);
        self.start(refused);
        self.jump(error, &[available], n);
        self.start(moved_out);
        self.frame_copy(block, frame, moved, n);
        self.set_carrier_word(header, carrier, SeqWord::Data, block, n);
        self.set_carrier_word(header, carrier, SeqWord::Cap, new_cap, n);
        self.inst(Inst::FrameRefund { charge: old_charge }, n);
        self.jump(success, &[], n);
        Some(())
    }
    fn frame_copy(&mut self, to: ValueId, from: ValueId, bytes: ValueId, n: NodeId) {
        let bytes_ty = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let to = self.address(AddressBase::Value(to), &[], bytes_ty, n);
        let from = self.address(AddressBase::Value(from), &[], bytes_ty, n);
        self.inst(Inst::Copy { to, from, bytes }, n);
    }
}
