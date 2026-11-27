//! Constant-stack clone of a recursive type (FAIL-17).
//!
//! A type that owns a value of itself (through a `Box`, an inline member, or the items
//! of a `List` or array) is copied by one machine over its owning component, never by
//! glue calls one level each. Each member has a start block taking the source and copy
//! places; it copies the node's bytes, then replaces each own part in order. Before a
//! part that leads back into the component, the frame (a node and its field, or a
//! sequence and its item) is pushed on the context's glue worklist, then the walk
//! descends; when the part is done, `dispatch` pops the frame and its resume block goes
//! on with the next part.
//!
//! An entry's frame has its parts before the entry's index complete; the part at the
//! index is in progress, and until it is replaced the copy's bytes still name the
//! original's (MEM-17), so a pointer equal to the original's was not yet replaced. A
//! refusal (`Capacity`, MEM-21, or a work step, MEM-30) unwinds: each popped frame
//! frees the block of its part in progress when the copy's pointer differs from the
//! original's, drops its complete parts, and frees a sequence's new block. So a refused
//! clone leaves the original intact and holds nothing, at any depth.
use super::*;
use hirsel_core_ir::RtOp;

struct Machine {
    members: Vec<TypeId>,
    starts: Vec<BlockId>,
    resumes: Vec<BlockId>,
    undos: Vec<Undo>,
    dispatch: BlockId,
    unwind: BlockId,
}

/// What a popped frame undoes.
enum Undo {
    /// Field `at` of a node of `u` (in `case`) in progress, the fields `done` complete.
    Node {
        u: TypeId,
        case: Option<u32>,
        done: Vec<(u32, FieldTy)>,
        at: (u32, FieldTy),
    },
    /// The item at the entry's index in progress in a new List block (or an array).
    Items { item: FieldTy, array: Option<u64> },
}

/// The owned edges of `ty`: types it holds inline, in a `Box`, or as sequence items. A
/// row with its own copy or a region row holds none: its copy is not a walk.
fn owned_edges(module: &Module, ty: TypeId) -> Vec<TypeId> {
    fn named(field: &FieldTy, out: &mut Vec<TypeId>) {
        match field {
            FieldTy::Inline(t) | FieldTy::Box(t) => out.push(TypeId(t.0)),
            FieldTy::Seq(item) | FieldTy::Array(item, _) => named(item, out),
            _ => {}
        }
    }
    let row = &module.types[ty.index()];
    let mut out = vec![];
    if row.clone.is_some() || row.repr == LRepr::Region {
        return out;
    }
    match &row.body {
        ShapeBody::Record(fs) => fs.iter().for_each(|f| named(&f.ty, &mut out)),
        ShapeBody::Variant(cs) => cs
            .iter()
            .flat_map(|c| c.fields.iter())
            .for_each(|f| named(&f.ty, &mut out)),
    }
    out
}

fn owned_reach(module: &Module, ty: TypeId) -> Vec<bool> {
    let mut seen = vec![false; module.types.len()];
    let mut work = owned_edges(module, ty);
    while let Some(t) = work.pop() {
        if !std::mem::replace(&mut seen[t.index()], true) {
            work.extend(owned_edges(module, t));
        }
    }
    seen
}

/// The owning component of `ty`: every type it owns that owns it back, `ty` first.
/// Empty when `ty` does not own itself.
pub(super) fn owned_component(module: &Module, ty: TypeId) -> Vec<TypeId> {
    let from = owned_reach(module, ty);
    if !from[ty.index()] {
        return vec![];
    }
    let mut members = vec![ty];
    for (i, _) in from.iter().enumerate().filter(|(_, r)| **r) {
        let u = TypeId(i as u32);
        if u != ty && owned_reach(module, u)[ty.index()] {
            members.push(u);
        }
    }
    members
}

impl Derive<'_> {
    /// The body of `$clone` at the recursive `ty`, from `from` into `to`.
    pub(super) fn clone_walk(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        from: ValueId,
        to: ValueId,
    ) -> Result<(), &'static str> {
        let members = owned_component(self.module, ty);
        let mark = self.runtime(c, RtOp::GlueMark, &[]).unwrap();
        let starts = members
            .iter()
            .map(|_| c.block(&[Ty::Ptr, Ty::Ptr]))
            .collect();
        let mut m = Machine {
            members,
            starts,
            resumes: vec![],
            undos: vec![],
            dispatch: c.block(&[]),
            unwind: c.block(&[]),
        };
        let saved = c.unwind.replace(m.unwind);
        c.jump(m.starts[0], &[from, to]);
        for i in 0..m.members.len() {
            c.start(m.starts[i]);
            let [a, b] = c.params(m.starts[i]);
            let u = m.members[i];
            self.clone_node(c, &mut m, u, a, b)?;
        }
        let finish = c.block(&[]);
        c.start(m.dispatch);
        let resumes = m.resumes.clone();
        self.pop_to(c, mark, &resumes, finish);
        c.start(m.unwind);
        let fail = c.block(&[]);
        let arms: Vec<BlockId> = m
            .undos
            .iter()
            .map(|_| c.block(&[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)]))
            .collect();
        self.pop_to(c, mark, &arms, fail);
        let undos = std::mem::take(&mut m.undos);
        for (undo, &arm) in undos.iter().zip(&arms) {
            c.start(arm);
            let ps = c.body.blocks[arm.index()]
                .params
                .get(&c.body.pools.values)
                .unwrap()
                .to_vec();
            self.undo(c, undo, ps[0], ps[1], ps[2])?;
            c.jump(m.unwind, &[]);
        }
        c.unwind = saved;
        c.start(fail);
        c.end(Terminator::Propagate);
        c.start(finish);
        ret(c, &[]);
        Ok(())
    }

    /// Pop the next frame above `mark` and switch on its resume number into `arms`;
    /// an empty worklist goes to `empty`.
    fn pop_to(&mut self, c: &mut Code, mark: ValueId, arms: &[BlockId], empty: BlockId) {
        if arms.is_empty() {
            c.jump(empty, &[]);
            return;
        }
        let e = self.runtime(c, RtOp::GluePop, &[mark]).unwrap();
        let null = c.value(Ty::Ptr);
        c.inst(Inst::Const {
            result: null,
            bits: 0,
        });
        let none = c.compare(CompareOp::Eq, e, null);
        let popped = c.block(&[]);
        c.branch(none, empty, popped);
        c.start(popped);
        let w0 = c.read(e, Ty::Ptr);
        let at = c.offset(e, 8);
        let w1 = c.read(at, Ty::Ptr);
        let at = c.offset(e, 16);
        let k = c.read(at, Ty::Scalar(Scalar::U64));
        let at = c.offset(e, 24);
        let w3 = c.read(at, Ty::Scalar(Scalar::U64));
        let edges: Vec<_> = arms
            .iter()
            .enumerate()
            .map(|(i, &r)| hirsel_core_ir::SwitchArm {
                value: i as u64,
                edge: c.edge(r, &[w0, w1, w3]),
            })
            .collect();
        let otherwise = edges.last().unwrap().edge;
        let range = PoolRange::push(
            &mut c.body.pools.arms,
            edges[..edges.len() - 1].iter().copied(),
        );
        c.end(Terminator::Switch {
            value: k,
            arms: range,
            otherwise,
        });
    }

    /// The node of member `u` from `a` into `b`: its bytes, then its own parts. Every
    /// path ends in `dispatch` or in a member's start.
    fn clone_node(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        u: TypeId,
        a: ValueId,
        b: ValueId,
    ) -> Result<(), &'static str> {
        let size = c.field_size(&FieldTy::Inline(TypeRef(u.0)));
        copy_value(c, b, a, size);
        match self.module.types[u.index()].body.clone() {
            ShapeBody::Record(fs) => {
                let fields = fs.into_iter().map(|f| f.ty).collect::<Vec<_>>();
                self.clone_walk_fields(c, m, u, None, &fields, a, b)
            }
            ShapeBody::Variant(cs) => {
                let address = c.address(a, PlaceTy::Declared(u));
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address,
                });
                let mut arms = vec![];
                for i in 0..cs.len() {
                    let target = c.block(&[]);
                    arms.push(hirsel_core_ir::SwitchArm {
                        value: i as u64,
                        edge: c.edge(target, &[]),
                    });
                }
                let otherwise = arms.last().unwrap().edge;
                let range = PoolRange::push(
                    &mut c.body.pools.arms,
                    arms[..arms.len() - 1].iter().copied(),
                );
                c.end(Terminator::Switch {
                    value: tag,
                    arms: range,
                    otherwise,
                });
                for (i, case) in cs.into_iter().enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    let fields = case.fields.into_iter().map(|f| f.ty).collect::<Vec<_>>();
                    self.clone_walk_fields(c, m, u, Some(i as u32), &fields, a, b)?;
                }
                Ok(())
            }
        }
    }

    /// Whether the copy of `field` descends into the component.
    fn descends(&self, m: &Machine, field: &FieldTy) -> bool {
        match field {
            FieldTy::Inline(t) | FieldTy::Box(t) => m.members.contains(&TypeId(t.0)),
            FieldTy::Seq(item) | FieldTy::Array(item, _) => self.descends(m, item),
            _ => false,
        }
    }

    fn field_place(c: &mut Code, u: TypeId, case: Option<u32>, p: ValueId, j: u32) -> ValueId {
        let base = c.address(p, PlaceTy::Declared(u));
        c.project_field_pointer(base, case, j)
    }

    /// The own parts of one record or case of `u` in order, then `dispatch`.
    #[allow(clippy::too_many_arguments)]
    fn clone_walk_fields(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        u: TypeId,
        case: Option<u32>,
        fields: &[FieldTy],
        mut a: ValueId,
        mut b: ValueId,
    ) -> Result<(), &'static str> {
        let mut done: Vec<(u32, FieldTy)> = vec![];
        for (j, field) in fields.iter().enumerate() {
            let j = j as u32;
            if !self.owns(field, &mut vec![]) {
                continue;
            }
            let x = Self::field_place(c, u, case, a, j);
            let y = Self::field_place(c, u, case, b, j);
            if !self.descends(m, field) {
                // A refusal inside drops this node's complete parts, then unwinds.
                let fail = c.block(&[]);
                let here = detour(c);
                c.start(fail);
                self.drop_done(c, u, case, &done, b)?;
                c.jump(m.unwind, &[]);
                c.start(here);
                self.clone_field(c, field, x, y, fail)?;
                done.push((j, field.clone()));
                continue;
            }
            let zero = c.constant(0);
            let undo = Undo::Node {
                u,
                case,
                done: done.clone(),
                at: (j, field.clone()),
            };
            let r = self.push_frame(c, m, [a, b, zero], undo);
            self.clone_descend(c, m, field, x, y)?;
            [a, b, _] = resume(c, r);
            done.push((j, field.clone()));
        }
        c.jump(m.dispatch, &[]);
        Ok(())
    }

    /// Push a frame with its resume and undo; returns the resume block.
    fn push_frame(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        words: [ValueId; 3],
        undo: Undo,
    ) -> BlockId {
        let r = c.block(&[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)]);
        let k = c.constant(m.resumes.len() as u64);
        m.resumes.push(r);
        m.undos.push(undo);
        self.runtime(c, RtOp::GluePush, &[words[0], words[1], k, words[2]]);
        r
    }

    /// Copy `field`, which descends, from `x` into `y`; the path ends.
    fn clone_descend(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        field: &FieldTy,
        x: ValueId,
        y: ValueId,
    ) -> Result<(), &'static str> {
        let start =
            |m: &Machine, t: &TypeRef| m.starts[m.members.iter().position(|u| u.0 == t.0).unwrap()];
        match field {
            FieldTy::Inline(t) => c.jump(start(m, t), &[x, y]),
            FieldTy::Box(t) => {
                // MEM-30 work steps: a node is one clone item, charged alone.
                let one = c.constant(1);
                self.clone_work(c, table::CLONE_ITEM, one, m.unwind);
                let charge = c.constant(self.box_charge(TypeId(t.0)));
                let size = c.field_size(&FieldTy::Inline(*t));
                let kind = AllocKind::own(field.clone());
                let block = alloc_kind(c, kind, charge, size, m.unwind);
                let old = c.read(x, Ty::Ptr);
                store(c, y, block);
                c.jump(start(m, t), &[old, block]);
            }
            FieldTy::Seq(item) => self.clone_items(c, m, item, None, x, y)?,
            FieldTy::Array(item, n) => self.clone_items(c, m, item, Some(*n), x, y)?,
            _ => unreachable!("only inline, box and sequence parts descend"),
        }
        Ok(())
    }

    /// A List (new block of capacity equal to its length) or array whose items descend.
    #[allow(clippy::too_many_arguments)]
    fn clone_items(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        item: &FieldTy,
        array: Option<u64>,
        x: ValueId,
        y: ValueId,
    ) -> Result<(), &'static str> {
        let u = Ty::Scalar(Scalar::U64);
        if array.is_none() {
            let at = c.offset(x, 8);
            let len = c.read(at, u);
            let zero = c.constant(0);
            let empty = c.compare(CompareOp::Eq, len, zero);
            let (none, some) = (c.block(&[]), c.block(&[]));
            c.branch(empty, none, some);
            // An empty copy allocates nothing (budgets.md).
            c.start(none);
            let null = c.value(Ty::Ptr);
            c.inst(Inst::Const {
                result: null,
                bits: 0,
            });
            store(c, y, null);
            let cap = c.offset(y, 16);
            store(c, cap, zero);
            c.jump(m.dispatch, &[]);
            c.start(some);
            // As a List's clone: work steps for its logical bytes and one per item,
            // before the block is admitted (MEM-30), charged `len` logical sizes.
            let logical = hirsel_core_layout::field_size(&self.module.types, &self.recipes, item).2;
            let logical = c.constant(logical);
            let units = c.mul(len, logical);
            self.clone_work(c, table::COPY, units, m.unwind);
            self.clone_work(c, table::CLONE_ITEM, len, m.unwind);
            let stride = c.field_size(item);
            let bytes = c.mul(len, stride);
            let charge = self.seq_charge(c, item, len);
            let kind = AllocKind::own(FieldTy::Seq(Box::new(item.clone())));
            let block = alloc_kind(c, kind, charge, bytes, m.unwind);
            let old = c.read(x, Ty::Ptr);
            copy_value(c, block, old, bytes);
            store(c, y, block);
            let cap = c.offset(y, 16);
            store(c, cap, len);
        }
        let head = c.block(&[Ty::Ptr, Ty::Ptr, u]);
        let body = c.block(&[]);
        let zero = c.constant(0);
        c.jump(head, &[x, y, zero]);
        let [hx, hy, i] = resume(c, head);
        let (dx, dy, len) = items(c, array, hx, hy);
        let more = c.compare(CompareOp::Lt, i, len);
        c.branch(more, body, m.dispatch);
        c.start(body);
        let stride = c.field_size(item);
        let offset = c.mul(i, stride);
        let px = c.offset_value(dx, offset);
        let py = c.offset_value(dy, offset);
        let undo = Undo::Items {
            item: item.clone(),
            array,
        };
        let r = self.push_frame(c, m, [hx, hy, i], undo);
        self.clone_descend(c, m, item, px, py)?;
        let [hx, hy, i] = resume(c, r);
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[hx, hy, next]);
        Ok(())
    }

    /// Drop the complete parts `done` of the copy `b` of a node of `u`, last first.
    fn drop_done(
        &mut self,
        c: &mut Code,
        u: TypeId,
        case: Option<u32>,
        done: &[(u32, FieldTy)],
        b: ValueId,
    ) -> Result<(), &'static str> {
        for (j, field) in done.iter().rev() {
            let y = Self::field_place(c, u, case, b, *j);
            self.drop_field(c, field, y)?;
        }
        Ok(())
    }

    /// Free `block` charged `charge` when it differs from the original's `old`.
    fn free_replaced(c: &mut Code, old: ValueId, block: ValueId, charge: ValueId) {
        let replaced = c.compare(CompareOp::Ne, old, block);
        let (yes, join) = (c.block(&[]), c.block(&[]));
        c.branch(replaced, yes, join);
        c.start(yes);
        c.inst(Inst::Free { block, charge });
        c.jump(join, &[]);
        c.start(join);
    }

    /// Undo a popped frame with words `w0`, `w1` and index `i`; the block stays open.
    fn undo(
        &mut self,
        c: &mut Code,
        undo: &Undo,
        w0: ValueId,
        w1: ValueId,
        i: ValueId,
    ) -> Result<(), &'static str> {
        match undo {
            Undo::Node { u, case, done, at } => {
                if let FieldTy::Box(t) = &at.1 {
                    let x = Self::field_place(c, *u, *case, w0, at.0);
                    let y = Self::field_place(c, *u, *case, w1, at.0);
                    let old = c.read(x, Ty::Ptr);
                    let block = c.read(y, Ty::Ptr);
                    let charge = c.constant(self.box_charge(TypeId(t.0)));
                    Self::free_replaced(c, old, block, charge);
                }
                self.drop_done(c, *u, *case, done, w1)
            }
            Undo::Items { item, array } => {
                let (dx, dy, _) = items(c, *array, w0, w1);
                if let FieldTy::Box(t) = item {
                    let stride = c.field_size(item);
                    let offset = c.mul(i, stride);
                    let px = c.offset_value(dx, offset);
                    let py = c.offset_value(dy, offset);
                    let old = c.read(px, Ty::Ptr);
                    let block = c.read(py, Ty::Ptr);
                    let charge = c.constant(self.box_charge(TypeId(t.0)));
                    Self::free_replaced(c, old, block, charge);
                }
                self.drop_items(c, item, dy, i)?;
                if array.is_none() {
                    let at = c.offset(w1, 16);
                    let cap = c.read(at, Ty::Scalar(Scalar::U64));
                    let charge = self.seq_charge(c, item, cap);
                    c.inst(Inst::Free { block: dy, charge });
                }
                Ok(())
            }
        }
    }
}

/// The data of the two headers and the length (a List's from the original's header).
fn items(
    c: &mut Code,
    array: Option<u64>,
    hx: ValueId,
    hy: ValueId,
) -> (ValueId, ValueId, ValueId) {
    match array {
        Some(n) => (hx, hy, c.constant(n)),
        None => {
            let dx = c.read(hx, Ty::Ptr);
            let dy = c.read(hy, Ty::Ptr);
            let at = c.offset(hx, 8);
            (dx, dy, c.read(at, Ty::Scalar(Scalar::U64)))
        }
    }
}

fn resume(c: &mut Code, r: BlockId) -> [ValueId; 3] {
    c.start(r);
    let ps = c.body.blocks[r.index()]
        .params
        .get(&c.body.pools.values)
        .unwrap();
    [ps[0], ps[1], ps[2]]
}
