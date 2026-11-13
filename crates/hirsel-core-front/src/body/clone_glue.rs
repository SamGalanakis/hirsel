//! MEM-17's own copy, `$clone[T](from, to) -> (requested, available)`, derived from
//! `T`'s layout recipe like the key glue.
//!
//! The copy starts as `T`'s bytes and then replaces each own part in place, depth
//! first: a `List` or `String` gets a new block of capacity equal to its length, a `Box`
//! a new node, a counted edge one more holder. Each block is admitted when the walk
//! reaches it, before what it holds (T1.5). A refusal returns its `requested` charge and
//! the room left, after dropping what this copy already holds in reverse; `requested`
//! is 0 exactly when the copy is complete, since a refused admission is never of 0
//! bytes. Until a part is replaced the copy's bytes still name the original's blocks,
//! so nothing drops a part that was not replaced.
use super::key_glue::{Code, Derive, KeyOp};
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy};

impl Derive<'_> {
    pub(super) fn clone_function(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        from: ValueId,
        to: ValueId,
    ) -> Result<(), &'static str> {
        let size = self.row_size(ty);
        copy(c, to, from, size);
        let fail = c.block(&[Ty::Scalar(Scalar::U64); 2]);
        self.clone_parts(c, ty, from, to, fail)?;
        let zero = c.constant(0);
        ret(c, &[zero, zero]);
        c.start(fail);
        let [requested, available] = c.params(fail);
        ret(c, &[requested, available]);
        Ok(())
    }
    fn row_size(&self, ty: TypeId) -> u64 {
        hirsel_core_layout::field_size(
            &self.module.types,
            &self.recipes,
            &FieldTy::Inline(TypeRef(ty.0)),
        )
        .0
    }
    fn size_of(&self, field: &FieldTy) -> u64 {
        hirsel_core_layout::field_size(&self.module.types, &self.recipes, field).0
    }
    /// Whether a value of `field` holds an own or counted part (seen breaks a cycle).
    fn owns(&self, field: &FieldTy, seen: &mut Vec<u32>) -> bool {
        match field {
            FieldTy::Seq(_)
            | FieldTy::Box(_)
            | FieldTy::Rc(_)
            | FieldTy::Arc(_)
            | FieldTy::Map(..)
            | FieldTy::OwnFn => true,
            FieldTy::Array(item, _) => self.owns(item, seen),
            FieldTy::Inline(t) => {
                if seen.contains(&t.0) {
                    return false;
                }
                seen.push(t.0);
                let body = self.module.types[t.0 as usize].body.clone();
                match body {
                    ShapeBody::Record(fs) => fs.iter().any(|f| self.owns(&f.ty, seen)),
                    ShapeBody::Variant(cs) => cs
                        .iter()
                        .any(|c| c.fields.iter().any(|f| self.owns(&f.ty, seen))),
                }
            }
            _ => false,
        }
    }
    fn offsets(&self, ty: TypeId) -> Vec<Vec<u64>> {
        match &self.recipes[ty.index()].layout {
            Layout::Fixed(l) => l.offsets.clone(),
            _ => unreachable!("clone glue takes value rows"),
        }
    }
    /// Replace the own parts of the copy at `to` of the `ty` at `from`; on a refusal
    /// drop the parts replaced so far and jump to `fail(requested, available)`.
    fn clone_parts(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        let offsets = self.offsets(ty);
        match self.module.types[ty.index()].body.clone() {
            ShapeBody::Record(fs) => {
                let fields: Vec<_> = fs.into_iter().map(|f| f.ty).collect();
                self.clone_fields(c, &fields, &offsets[0], from, to, fail)
            }
            ShapeBody::Variant(cs) => {
                let at = c.address(from, PlaceTy::Declared(ty));
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address: at,
                });
                let done = c.block(&[]);
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
                    let fields: Vec<_> = case.fields.into_iter().map(|f| f.ty).collect();
                    self.clone_fields(c, &fields, &offsets[i], from, to, fail)?;
                    c.jump(done, &[]);
                }
                c.start(done);
                Ok(())
            }
        }
    }
    /// The fields at `offsets`, in declaration order; a refusal at field k drops
    /// fields k-1 down to 0 before `fail`.
    fn clone_fields(
        &mut self,
        c: &mut Code,
        fields: &[FieldTy],
        offsets: &[u64],
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        let mut undo = fail;
        for (f, &off) in fields.iter().zip(offsets) {
            if !self.owns(f, &mut vec![]) {
                continue;
            }
            let (x, y) = (c.offset(from, off), c.offset(to, off));
            self.clone_field(c, f, x, y, undo)?;
            // A later refusal drops this field, then what came before it.
            let next = c.block(&[Ty::Scalar(Scalar::U64); 2]);
            let here = detour(c);
            c.start(next);
            let [requested, available] = c.params(next);
            self.drop_field(c, f, y)?;
            c.jump(undo, &[requested, available]);
            c.start(here);
            undo = next;
        }
        Ok(())
    }
    fn clone_field(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        match field {
            FieldTy::Inline(t) => {
                let f = self.function(TypeId(t.0), KeyOp::Clone)?;
                let x = c.address(from, PlaceTy::Declared(TypeId(t.0)));
                let y = c.address(to, PlaceTy::Declared(TypeId(t.0)));
                self.call_checked(c, f, x, y, fail, None);
            }
            FieldTy::Box(t) => {
                let t = TypeId(t.0);
                let size = self.row_size(t);
                let block = alloc(c, AllocKind::Own, round8(size), size, fail);
                let f = self.function(t, KeyOp::Clone)?;
                let old = c.read(from, Ty::Ptr);
                let x = c.address(old, PlaceTy::Declared(t));
                let y = c.address(block, PlaceTy::Declared(t));
                self.call_checked(c, f, x, y, fail, Some((block, round8(size))));
                store(c, to, block);
            }
            FieldTy::Rc(_) | FieldTy::Arc(_) => {
                let node = c.read(from, Ty::Ptr);
                c.inst(Inst::Share {
                    node,
                    kind: counted(field),
                });
            }
            FieldTy::Seq(item) => {
                let len_at = c.offset(from, 8);
                let len = c.read(len_at, Ty::Scalar(Scalar::U64));
                let zero = c.constant(0);
                let empty = c.compare(CompareOp::Eq, len, zero);
                let (none, some, done) = (c.block(&[]), c.block(&[]), c.block(&[]));
                c.branch(empty, none, some);
                // An empty copy allocates nothing (budgets.md).
                c.start(none);
                let null = c.value(Ty::Ptr);
                c.inst(Inst::Const {
                    result: null,
                    bits: 0,
                });
                store(c, to, null);
                let cap = c.offset(to, 16);
                store(c, cap, zero);
                c.jump(done, &[]);
                c.start(some);
                let stride = self.size_of(item);
                let k = c.constant(stride);
                let bytes = c.mul(len, k);
                let charge = round8_value(c, bytes);
                let block = alloc_value(c, charge, bytes, fail);
                let old = c.read(from, Ty::Ptr);
                copy_value(c, block, old, bytes);
                store(c, to, block);
                let cap = c.offset(to, 16);
                store(c, cap, len);
                if self.owns(item, &mut vec![]) {
                    // A refusal inside drops the elements already copied, then the block.
                    let undo = c.block(&[Ty::Scalar(Scalar::U64); 3]);
                    self.each(c, item, old, block, len, undo)?;
                    let here = detour(c);
                    c.start(undo);
                    let ps = c.body.blocks[undo.index()]
                        .params
                        .get(&c.body.pools.values)
                        .unwrap()
                        .to_vec();
                    self.drop_items(c, item, block, ps[2])?;
                    c.inst(Inst::Free { block, charge });
                    c.jump(fail, &[ps[0], ps[1]]);
                    c.start(here);
                }
                c.jump(done, &[]);
                c.start(done);
            }
            FieldTy::Array(item, n) => {
                let len = c.constant(*n);
                let undo = c.block(&[Ty::Scalar(Scalar::U64); 3]);
                self.each(c, item, from, to, len, undo)?;
                let here = detour(c);
                c.start(undo);
                let ps = c.body.blocks[undo.index()]
                    .params
                    .get(&c.body.pools.values)
                    .unwrap()
                    .to_vec();
                self.drop_items(c, item, to, ps[2])?;
                c.jump(fail, &[ps[0], ps[1]]);
                c.start(here);
            }
            FieldTy::Map(..) => return Err("a clone of a Map is not built"),
            FieldTy::OwnFn => return Err("a clone of an owning closure is not built"),
            _ => {}
        }
        Ok(())
    }
    /// Clone each of `len` items at `from` into `to`; a refusal at item i jumps to
    /// `undo(requested, available, i)`.
    fn each(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        from: ValueId,
        to: ValueId,
        len: ValueId,
        undo: BlockId,
    ) -> Result<(), &'static str> {
        let zero = c.constant(0);
        let head = c.block(&[Ty::Scalar(Scalar::U64)]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        c.jump(head, &[zero]);
        c.start(head);
        let i = c.params(head)[0];
        let on = c.compare(CompareOp::Lt, i, len);
        c.branch(on, body, done);
        c.start(body);
        let stride = c.constant(self.size_of(item));
        let at = c.mul(i, stride);
        let x = c.offset_value(from, at);
        let y = c.offset_value(to, at);
        let fail = c.block(&[Ty::Scalar(Scalar::U64); 2]);
        self.clone_field(c, item, x, y, fail)?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[next]);
        c.start(fail);
        let [requested, available] = c.params(fail);
        c.jump(undo, &[requested, available, i]);
        c.start(done);
        Ok(())
    }
    /// Drop the first `count` items at `at`, last first.
    fn drop_items(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        at: ValueId,
        count: ValueId,
    ) -> Result<(), &'static str> {
        let head = c.block(&[Ty::Scalar(Scalar::U64)]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        c.jump(head, &[count]);
        c.start(head);
        let i = c.params(head)[0];
        let zero = c.constant(0);
        let on = c.compare(CompareOp::Gt, i, zero);
        c.branch(on, body, done);
        c.start(body);
        let one = c.constant(1);
        let j = c.binary(BinaryOp::Sub, i, one);
        let stride = c.constant(self.size_of(item));
        let off = c.mul(j, stride);
        let p = c.offset_value(at, off);
        self.drop_field(c, item, p)?;
        c.jump(head, &[j]);
        c.start(done);
        Ok(())
    }
    /// Clean a complete copy of `field` at `at`.
    fn drop_field(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        at: ValueId,
    ) -> Result<(), &'static str> {
        match field {
            FieldTy::Inline(t) => {
                let address = c.address(at, PlaceTy::Declared(TypeId(t.0)));
                c.inst(Inst::Drop {
                    address,
                    ty: TypeId(t.0),
                    fail: None,
                });
            }
            FieldTy::Box(t) => {
                let t = TypeId(t.0);
                let block = c.read(at, Ty::Ptr);
                let address = c.address(block, PlaceTy::Declared(t));
                c.inst(Inst::Drop {
                    address,
                    ty: t,
                    fail: None,
                });
                let charge = c.constant(round8(self.row_size(t)));
                c.inst(Inst::Free { block, charge });
            }
            FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = c.read(at, Ty::Ptr);
                c.inst(Inst::Release {
                    node,
                    kind: counted(field),
                    ty: TypeId(t.0),
                });
            }
            FieldTy::Seq(item) => {
                let block = c.read(at, Ty::Ptr);
                let cap_at = c.offset(at, 16);
                let cap = c.read(cap_at, Ty::Scalar(Scalar::U64));
                let zero = c.constant(0);
                let held = c.compare(CompareOp::Ne, cap, zero);
                let (yes, done) = (c.block(&[]), c.block(&[]));
                c.branch(held, yes, done);
                c.start(yes);
                if self.owns(item, &mut vec![]) {
                    let len_at = c.offset(at, 8);
                    let len = c.read(len_at, Ty::Scalar(Scalar::U64));
                    self.drop_items(c, item, block, len)?;
                }
                let stride = c.constant(self.size_of(item));
                let bytes = c.mul(cap, stride);
                let charge = round8_value(c, bytes);
                c.inst(Inst::Free { block, charge });
                c.jump(done, &[]);
                c.start(done);
            }
            FieldTy::Array(item, n) => {
                let n = c.constant(*n);
                self.drop_items(c, item, at, n)?;
            }
            _ => {}
        }
        Ok(())
    }
    /// Call `$clone` and go on when it completed; otherwise free `block` (when the copy
    /// was going into a new node) and jump to `fail` with its outcome.
    fn call_checked(
        &self,
        c: &mut Code,
        f: FuncId,
        from: AddressId,
        to: AddressId,
        fail: BlockId,
        block: Option<(ValueId, u64)>,
    ) {
        let signature = self.module.functions[f.index()].signature;
        let requested = c.value(Ty::Scalar(Scalar::U64));
        let available = c.value(Ty::Scalar(Scalar::U64));
        let args = PoolRange::push(
            &mut c.body.pools.args,
            [Arg::Address(from), Arg::Address(to)],
        );
        let results = PoolRange::push(&mut c.body.pools.values, [requested, available]);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee: Callee::Direct(f),
            signature,
            args,
            results,
            fail: None,
        });
        c.inst(Inst::Call { call });
        let zero = c.constant(0);
        let done = c.compare(CompareOp::Eq, requested, zero);
        let (yes, no) = (c.block(&[]), c.block(&[]));
        c.branch(done, yes, no);
        c.start(no);
        if let Some((block, charge)) = block {
            let charge = c.constant(charge);
            c.inst(Inst::Free { block, charge });
        }
        c.jump(fail, &[requested, available]);
        c.start(yes);
    }
}

fn counted(field: &FieldTy) -> hirsel_core_ir::Counted {
    match field {
        FieldTy::Arc(_) => hirsel_core_ir::Counted::Arc,
        _ => hirsel_core_ir::Counted::Rc,
    }
}
fn round8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}
fn round8_value(c: &mut Code, bytes: ValueId) -> ValueId {
    let seven = c.constant(7);
    let up = c.binary(BinaryOp::Add, bytes, seven);
    let mask = c.constant(!7);
    c.binary(BinaryOp::BitAnd, up, mask)
}
/// End the open block with a jump to a new one and return it, so another block can be
/// written before the walk goes on there.
fn detour(c: &mut Code) -> BlockId {
    let here = c.block(&[]);
    c.jump(here, &[]);
    here
}
fn ret(c: &mut Code, values: &[ValueId]) {
    let returns = PoolRange::push(&mut c.body.pools.values, values.iter().copied());
    c.end(Terminator::Return(returns));
}
fn store(c: &mut Code, p: ValueId, value: ValueId) {
    let ty = c.body.values[value.index()];
    let address = c.address(p, PlaceTy::Value(ty));
    c.inst(Inst::Store { address, value });
}
fn copy(c: &mut Code, to: ValueId, from: ValueId, size: u64) {
    let bytes = c.constant(size);
    copy_value(c, to, from, bytes);
}
fn copy_value(c: &mut Code, to: ValueId, from: ValueId, bytes: ValueId) {
    let to = c.address(to, PlaceTy::Value(Ty::Scalar(Scalar::U8)));
    let from = c.address(from, PlaceTy::Value(Ty::Scalar(Scalar::U8)));
    c.inst(Inst::Copy { to, from, bytes });
}
fn alloc(c: &mut Code, kind: AllocKind, charge: u64, size: u64, fail: BlockId) -> ValueId {
    let charge = c.constant(charge);
    let size = c.constant(size);
    alloc_kind(c, kind, charge, size, fail)
}
fn alloc_value(c: &mut Code, charge: ValueId, size: ValueId, fail: BlockId) -> ValueId {
    alloc_kind(c, AllocKind::Own, charge, size, fail)
}
/// Admit `charge` in the current limit; a refusal jumps to `fail(charge, available)`.
fn alloc_kind(
    c: &mut Code,
    kind: AllocKind,
    charge: ValueId,
    size: ValueId,
    fail: BlockId,
) -> ValueId {
    let result = c.value(Ty::Ptr);
    let available = c.value(Ty::Scalar(Scalar::U64));
    c.inst(Inst::Alloc {
        result,
        available,
        region_entry: None,
        kind,
        place: AllocPlace::Current,
        charge,
        overflow: None,
        size,
        align: 8,
        fail: None,
    });
    let null = c.value(Ty::Ptr);
    c.inst(Inst::Const {
        result: null,
        bits: 0,
    });
    let admitted = c.compare(CompareOp::Ne, result, null);
    let (yes, no) = (c.block(&[]), c.block(&[]));
    c.branch(admitted, yes, no);
    c.start(no);
    c.jump(fail, &[charge, available]);
    c.start(yes);
    result
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// `clone(x)` expecting `u` (MEM-17): the part of `x` that is `u`, through `Box`,
    /// `Rc` and `Arc` payloads outermost first, copied by `$clone[u]` into the `Ok`
    /// payload of the result.
    pub(super) fn clone_value(
        &mut self,
        x: TyId,
        u: TyId,
        v: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let mut part = self.bare(x);
        let u = self.bare(u);
        let mut at = match self.rep(part).ok()? {
            Rep::Place(_) => v,
            _ => {
                let slot = self.slot(part, Some(v), n)?;
                self.pointer(slot, n)
            }
        };
        while part != u {
            let TyKind::Adt(d, xs) = self.t.kind(part).clone() else {
                self.gap(n, "a clone into the twin type is not built");
                return None;
            };
            let skip = match self.p.decls[d.index()].role {
                Some(Role::Box) => 0,
                Some(Role::Rc | Role::Arc) => 8,
                _ => {
                    self.gap(n, "a clone into the twin type is not built");
                    return None;
                }
            };
            let block = self.get_word(AddressBase::Value(at), 0, Ty::Ptr, n)?;
            at = if skip == 0 {
                block
            } else {
                let skip = self.u64k(skip, n)?;
                let to = self.address(
                    AddressBase::Value(block),
                    &[Step::Byte(skip)],
                    PlaceTy::Value(Ty::Scalar(Scalar::U8)),
                    n,
                );
                self.pointer(to, n)
            };
            part = self.bare(xs[0]);
        }
        let (id, function, _) = self.key_function(KeyOp::Clone, u, n)?;
        let rid = self.declared(result)?;
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(rid), n);
        let from = self.address(AddressBase::Value(at), &[], PlaceTy::Declared(id), n);
        let ok = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(id),
            n,
        );
        let signature = self.e.module.functions[function.index()].signature;
        let requested = self.value(Ty::Scalar(Scalar::U64));
        let available = self.value(Ty::Scalar(Scalar::U64));
        let args = PoolRange::push(
            &mut self.b.pools.args,
            [Arg::Address(from), Arg::Address(ok)],
        );
        let results = PoolRange::push(&mut self.b.pools.values, [requested, available]);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(function),
            signature,
            args,
            results,
            fail: None,
        });
        self.inst(Inst::Call { call }, n);
        let zero = self.u64k(0, n)?;
        let done = self.test(CompareOp::Eq, requested, zero, n);
        let (yes, no, join) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(done, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.capacity_at(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            requested,
            available,
            n,
        )?;
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
}
