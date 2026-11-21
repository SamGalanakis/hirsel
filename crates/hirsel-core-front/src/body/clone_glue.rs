//! MEM-17's own copy, `$clone[T](from, to)`, derived from
//! `T`'s layout recipe like the key glue.
//!
//! The copy starts as `T`'s bytes and then replaces each own part in place, depth
//! first: a `List` or `String` gets a new block of capacity equal to its length, a `Box`
//! a new node, a counted edge one more holder. Each block is admitted when the walk
//! reaches it, before what it holds (T1.5). A refusal raises `Capacity` (MEM-21), and its
//! cleanup drops what this copy already holds in reverse. Until a part is replaced the
//! copy's bytes still name the original's blocks, so nothing drops a part that was not
//! replaced.
//!
//! A core row whose storage is not its structure (a `Deque`'s slots, a `Map`'s index)
//! declares its own copy, `clone[..](x: T) { .. }` (`Shape::clone`): the row's `$clone` is its
//! byte copy and then that body, at any depth, never a walk of its fields.
use super::key_glue::{Code, Derive, KeyOp};
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy};

impl Derive<'_> {
    // Slot kernels describe their live set outside the structural fields. The
    // core clone hook must own that walk, including inside a containing value.
    fn structural_clone_allowed(&self, ty: TypeId, allow_hooks: bool) -> Result<(), &'static str> {
        fn row(
            types: &[hirsel_core_layout::Shape],
            id: u32,
            seen: &mut Vec<u32>,
            allow_hooks: bool,
        ) -> bool {
            if seen.contains(&id) {
                return false;
            }
            seen.push(id);
            let shape = &types[id as usize];
            if shape.clone.is_some() {
                return !allow_hooks;
            }
            let slots = shape.name.starts_with("core@1/")
                && shape
                    .name
                    .split('[')
                    .next()
                    .is_some_and(|n| n.ends_with("/Slots"));
            if slots
                || shape.finalizer.is_some() && shape.repr != hirsel_core_layout::Repr::Resource
            {
                return true;
            }
            match &shape.body {
                ShapeBody::Record(fs) => fs.iter().any(|f| field(types, &f.ty, seen, allow_hooks)),
                ShapeBody::Variant(cs) => cs.iter().any(|c| {
                    c.fields
                        .iter()
                        .any(|f| field(types, &f.ty, seen, allow_hooks))
                }),
            }
        }
        fn field(
            types: &[hirsel_core_layout::Shape],
            ty: &FieldTy,
            seen: &mut Vec<u32>,
            allow_hooks: bool,
        ) -> bool {
            match ty {
                FieldTy::Inline(t)
                | FieldTy::Box(t)
                | FieldTy::Rc(t)
                | FieldTy::Arc(t)
                | FieldTy::RegionRef(t)
                | FieldTy::Link(t) => row(types, t.0, seen, allow_hooks),
                FieldTy::Seq(x)
                | FieldTy::Array(x, _)
                | FieldTy::View(x)
                | FieldTy::FixedSpan(x)
                | FieldTy::SeqSpan(x)
                | FieldTy::IndexedSpan(x)
                | FieldTy::Locator(x) => field(types, x, seen, allow_hooks),
                FieldTy::Map(k, v) => {
                    field(types, k, seen, allow_hooks) || field(types, v, seen, allow_hooks)
                }
                FieldTy::Scalar(_)
                | FieldTy::Fn
                | FieldTy::OwnFn
                | FieldTy::Borrow
                | FieldTy::Handle(_)
                | FieldTy::Text
                | FieldTy::FrozenBytes => false,
            }
        }
        if row(&self.module.types, ty.0, &mut vec![], allow_hooks) {
            Err("core-declared clone bodies for slot storage and core finalizers are not built")
        } else {
            Ok(())
        }
    }

    pub(super) fn clone_function(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        from: ValueId,
        to: ValueId,
    ) -> Result<(), &'static str> {
        self.structural_clone_allowed(ty, true)?;
        if self.module.types[ty.index()].repr == LRepr::Region {
            // A clone keeps the regions of its region parts (MEM-17): a region value,
            // a root or an element of a cloned container, is copied as its holder,
            // the block pointer, and its block is shared, not copied (FORM #117).
            copy(c, to, from, hirsel_core_layout::POINTER);
            ret(c, &[]);
            return Ok(());
        }
        let size = c.field_size(&FieldTy::Inline(TypeRef(ty.0)));
        copy_value(c, to, from, size);
        if let Some(f) = self.module.types[ty.index()].clone {
            let f = FuncId(f.0);
            let signature = self.module.functions[f.index()].signature;
            let args = PoolRange::push(&mut c.body.pools.args, [Arg::Value(from), Arg::Value(to)]);
            let results = PoolRange::push(&mut c.body.pools.values, []);
            let call = CallId(c.body.calls.len() as u32);
            c.body.calls.push(CallSite {
                callee: Callee::Direct(f),
                signature,
                args,
                results,
                fail: None,
                cold: false,
            });
            c.inst(Inst::Call { call });
            ret(c, &[]);
            return Ok(());
        }
        let fail = c.block(&[]);
        self.clone_parts(c, ty, from, to, fail)?;
        ret(c, &[]);
        c.start(fail);
        c.end(Terminator::Propagate);
        Ok(())
    }
    /// A box's charge is its target's logical size rounded up to 8 (MEM-24).
    fn box_charge(&self, t: TypeId) -> u64 {
        let (repr, recipe) = (self.module.types[t.index()].repr, &self.recipes[t.index()]);
        let logical = held_size(repr, recipe, true).expect("a boxed target has a logical size");
        round8(logical)
    }
    /// The charge of `count` elements of `item` in a List block: their logical
    /// sizes, rounded up to 8 (MEM-24), never their native strides.
    fn seq_charge(&self, c: &mut Code, item: &FieldTy, count: ValueId) -> ValueId {
        let logical = hirsel_core_layout::field_size(&self.module.types, &self.recipes, item).2;
        let l = c.constant(logical);
        let units = c.mul(count, l);
        round8_value(c, units)
    }
    /// Whether a value of `field` holds an own or counted part (seen breaks a cycle).
    fn owns(&self, field: &FieldTy, seen: &mut Vec<u32>) -> bool {
        match field {
            FieldTy::Seq(_)
            | FieldTy::FrozenBytes
            | FieldTy::Box(_)
            | FieldTy::Rc(_)
            | FieldTy::Arc(_)
            | FieldTy::Map(..)
            | FieldTy::OwnFn => true,
            FieldTy::Array(item, _) => self.owns(item, seen),
            FieldTy::Inline(t) => {
                if self.module.types[t.0 as usize].clone.is_some() {
                    return true;
                }
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
    /// Replace the own parts of the copy at `to` of the `ty` at `from`; on a refusal
    /// drop the parts replaced so far and go on to the cleanup `fail`.
    fn clone_parts(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        let places = [from, to].map(|p| c.address(p, PlaceTy::Declared(ty)));
        match self.module.types[ty.index()].body.clone() {
            ShapeBody::Record(fs) => self.clone_fields(c, &fs, places, None, fail),
            ShapeBody::Variant(cs) => {
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address: places[0],
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
                    self.clone_fields(c, &case.fields, places, Some(i as u32), fail)?;
                    c.jump(done, &[]);
                }
                c.start(done);
                Ok(())
            }
        }
    }
    /// A refusal at field k drops fields k-1 down to 0 before `fail`.
    fn clone_fields(
        &mut self,
        c: &mut Code,
        fields: &[hirsel_core_layout::Field],
        places: [AddressId; 2],
        case: Option<u32>,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        let mut undo = fail;
        for (i, field) in fields.iter().enumerate() {
            let f = &field.ty;
            if !self.owns(f, &mut vec![]) {
                continue;
            }
            let [x, y] = places.map(|base| c.project_field_pointer(base, case, i as u32));
            self.clone_field(c, f, x, y, undo)?;
            // A later refusal drops this field, then what came before it.
            let next = c.block(&[]);
            let here = detour(c);
            c.start(next);
            self.drop_field(c, f, y)?;
            c.jump(undo, &[]);
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
                let charge = self.box_charge(t);
                let charged = c.constant(charge);
                let size = c.field_size(&FieldTy::Inline(TypeRef(t.0)));
                let kind = AllocKind::Own {
                    edge: field.clone(),
                };
                let block = alloc_kind(c, kind, charged, size, fail);
                let f = self.function(t, KeyOp::Clone)?;
                let old = c.read(from, Ty::Ptr);
                let x = c.address(old, PlaceTy::Declared(t));
                let y = c.address(block, PlaceTy::Declared(t));
                self.call_checked(c, f, x, y, fail, Some((block, charge)));
                store(c, to, block);
            }
            FieldTy::Rc(_) | FieldTy::Arc(_) => {
                let node = c.read(from, Ty::Ptr);
                c.inst(Inst::Share {
                    node,
                    kind: counted(field),
                });
            }
            FieldTy::Seq(_) | FieldTy::FrozenBytes => {
                let u = Ty::Scalar(Scalar::U64);
                let byte = FieldTy::Scalar(Scalar::U8);
                let (item, len) = if let FieldTy::Seq(item) = field {
                    let at = c.offset(from, 8);
                    (&**item, c.read(at, u))
                } else {
                    let at = c.offset(from, 8);
                    let meta = c.read(at, u);
                    let one = c.constant(1);
                    let tag = c.binary(BinaryOp::BitAnd, meta, one);
                    let zero = c.constant(0);
                    let static_data = c.compare(CompareOp::Ne, tag, zero);
                    let (static_len, heap_len, join) = (c.block(&[]), c.block(&[]), c.block(&[u]));
                    c.branch(static_data, static_len, heap_len);
                    c.start(static_len);
                    let at = c.offset(from, 16);
                    let len = c.read(at, u);
                    c.jump(join, &[len]);
                    c.start(heap_len);
                    let len = c.binary(BinaryOp::Shr, meta, one);
                    c.jump(join, &[len]);
                    c.start(join);
                    (&byte, c.params(join)[0])
                };
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
                if matches!(field, FieldTy::FrozenBytes) {
                    let meta = c.offset(to, 8);
                    store(c, meta, zero);
                }
                c.jump(done, &[]);
                c.start(some);
                // The block holds `len` native strides and is charged `len` logical
                // sizes (MEM-24), as a List's own growth charges it.
                let k = c.field_size(item);
                let bytes = c.mul(len, k);
                let charge = self.seq_charge(c, item, len);
                let kind = AllocKind::Own {
                    edge: FieldTy::Seq(Box::new(item.clone())),
                };
                let block = alloc_kind(c, kind, charge, bytes, fail);
                let old = c.read(from, Ty::Ptr);
                copy_value(c, block, old, bytes);
                store(c, to, block);
                let cap = c.offset(to, 16);
                store(c, cap, len);
                if matches!(field, FieldTy::FrozenBytes) {
                    let meta = c.offset(to, 8);
                    let two = c.constant(2);
                    let length = c.mul(len, two);
                    store(c, meta, length);
                }
                if self.owns(item, &mut vec![]) {
                    // A refusal inside drops the elements already copied, then the block.
                    self.each(c, item, old, block, len, Some(charge), fail)?;
                }
                c.jump(done, &[]);
                c.start(done);
            }
            FieldTy::Array(item, n) => {
                let len = c.constant(*n);
                self.each(c, item, from, to, len, None, fail)?;
            }
            FieldTy::Map(..) => return Err("a clone of a Map is not built"),
            FieldTy::OwnFn => return Err("a clone of an owning closure is not built"),
            _ => {}
        }
        Ok(())
    }
    /// Clone each of `len` items at `from` into `to`; a refusal at item i drops items
    /// i-1 down to 0, frees the block `to` charged `free` when it is new, and goes on
    /// to the cleanup `undo`.
    #[allow(clippy::too_many_arguments)]
    fn each(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        from: ValueId,
        to: ValueId,
        len: ValueId,
        free: Option<ValueId>,
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
        let stride = c.field_size(item);
        let at = c.mul(i, stride);
        let x = c.offset_value(from, at);
        let y = c.offset_value(to, at);
        let fail = c.block(&[]);
        self.clone_field(c, item, x, y, fail)?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[next]);
        c.start(fail);
        self.drop_items(c, item, to, i)?;
        if let Some(charge) = free {
            c.inst(Inst::Free { block: to, charge });
        }
        c.jump(undo, &[]);
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
        let stride = c.field_size(item);
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
                let charge = c.constant(self.box_charge(t));
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
            FieldTy::Seq(_) | FieldTy::FrozenBytes => {
                // A completed FrozenBytes clone owns heap backing.
                let byte = FieldTy::Scalar(Scalar::U8);
                let item = if let FieldTy::Seq(item) = field {
                    &**item
                } else {
                    &byte
                };
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
                let charge = self.seq_charge(c, item, cap);
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
    /// Call `$clone`; its failure frees `block` (when the copy was going into a new
    /// node) and goes on to the cleanup `fail`.
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
        let fail = match block {
            None => fail,
            Some((block, charge)) => {
                let undo = c.block(&[]);
                let here = detour(c);
                c.start(undo);
                let charge = c.constant(charge);
                c.inst(Inst::Free { block, charge });
                c.jump(fail, &[]);
                c.start(here);
                undo
            }
        };
        let args = PoolRange::push(
            &mut c.body.pools.args,
            [Arg::Address(from), Arg::Address(to)],
        );
        let results = PoolRange::push(&mut c.body.pools.values, []);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee: Callee::Direct(f),
            signature,
            args,
            results,
            fail: Some(fail),
            cold: false,
        });
        c.inst(Inst::Call { call });
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
/// Admit from the current limit. A refusal raises `Capacity` with cleanup `fail`.
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
    c.end(Terminator::Raise {
        cause: hirsel_core_ir::RaiseCause::Capacity {
            requested: charge,
            available,
        },
        cleanup: Some(fail),
    });
    c.start(yes);
    result
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// The address of the first part of `x` matching `u`, through `Box`, `Rc` and
    /// `Arc` payloads outermost first (MEM-17).
    fn clone_part(&mut self, x: TyId, u: TyId, v: ValueId, n: NodeId) -> Option<ValueId> {
        let mut part = self.bare(x);
        let mut at = match self.rep_at(part, n)? {
            Rep::Place(_) => v,
            _ => {
                let slot = self.slot(part, Some(v), n)?;
                self.pointer(slot, n)
            }
        };
        loop {
            if part == u {
                return Some(at);
            }
            let TyKind::Adt(d, xs) = self.t.kind(part).clone() else {
                break;
            };
            let skip = match self.p.decls[d.index()].role {
                Some(Role::Box) => 0,
                Some(Role::Rc | Role::Arc) => 8,
                _ => break,
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
        unreachable!("MEM-17 matches a clone to its expected type before lowering")
    }
    pub(super) fn clone_value(
        &mut self,
        x: TyId,
        u: TyId,
        v: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = self.bare(u);
        let at = self.clone_part(x, u, v, n)?;
        let (id, function, _) = self.key_function(KeyOp::Clone, u, n)?;
        let rid = self.place_row(result, "a built-in result without a place is not built", n)?;
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(rid), n);
        let from = self.address(AddressBase::Value(at), &[], PlaceTy::Declared(id), n);
        let signature = self.e.source_signature(function);
        let fail = self.fail_to(n);
        let args = PoolRange::push(
            &mut self.b.pools.args,
            [Arg::Address(from), Arg::Address(to)],
        );
        let results = PoolRange::push(&mut self.b.pools.values, []);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(function),
            signature,
            args,
            results,
            fail,
            cold: false,
        });
        self.inst(Inst::Call { call }, n);
        if !self.live() {
            return None;
        }
        Some(self.pointer(to, n))
    }
}

#[cfg(test)]
mod kernel_clone_tests {
    use super::*;
    use hirsel_core_layout::{Carried, Field, FuncRef, Repr, Shape, TypeRef};

    #[test]
    fn every_clone_walk_gaps_on_kernel_storage_and_finalizers() {
        let field = |name: &str, ty| Field {
            name: name.into(),
            ty,
        };
        let row = |name: &str, fields, finalizer| Shape {
            name: name.into(),
            repr: Repr::Own,
            carried: Carried::Address,
            body: ShapeBody::Record(fields),
            finalizer,
            clone: None,
            codec: None,
            grant: 0,
            send: false,
            held: None,
        };
        let mut base = Module::default();
        base.types = vec![
            row(
                "Plain",
                vec![field("x", FieldTy::Scalar(Scalar::U64))],
                None,
            ),
            row(
                "core@1/list/Slots[u64]",
                vec![field(
                    "raw",
                    FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U64))),
                )],
                None,
            ),
            row(
                "RecursiveWrapper",
                vec![
                    field("self", FieldTy::Box(TypeRef(2))),
                    field(
                        "values",
                        FieldTy::Seq(Box::new(FieldTy::Inline(TypeRef(1)))),
                    ),
                ],
                None,
            ),
            row(
                "core@1/test/Guard",
                vec![field("x", FieldTy::Scalar(Scalar::U64))],
                Some(FuncRef(0)),
            ),
            row(
                "OrdinaryList",
                vec![field(
                    "raw",
                    FieldTy::Seq(Box::new(FieldTy::Inline(TypeRef(0)))),
                )],
                None,
            ),
        ];
        for ty in [1, 2, 3] {
            let error = super::super::key_glue::ensure(&mut base.clone(), TypeId(ty), KeyOp::Clone)
                .unwrap_err();
            assert_eq!(
                error,
                "core-declared clone bodies for slot storage and core finalizers are not built"
            );
        }
        super::super::key_glue::ensure(&mut base, TypeId(4), KeyOp::Clone).unwrap();
    }
}
