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
                | FieldTy::Stable(t)
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
                | FieldTy::Text => false,
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
        let size = self.row_size(ty);
        copy(c, to, from, size);
        if let Some(f) = self.module.types[ty.index()].clone {
            let requested = c.value(Ty::Scalar(Scalar::U64));
            let available = c.value(Ty::Scalar(Scalar::U64));
            let f = FuncId(f.0);
            let signature = self.module.functions[f.index()].signature;
            let args = PoolRange::push(&mut c.body.pools.args, [Arg::Value(from), Arg::Value(to)]);
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
            ret(c, &[requested, available]);
            return Ok(());
        }
        let fail = c.block(&[Ty::Scalar(Scalar::U64); 2]);
        self.clone_parts(c, ty, from, to, fail)?;
        let zero = c.constant(0);
        ret(c, &[zero, zero]);
        c.start(fail);
        let [requested, available] = c.params(fail);
        ret(c, &[requested, available]);
        Ok(())
    }
    /// A row whose copy is its declared clone body cannot be walked field by field
    /// into or out of a twin.
    fn walkable(&self, ty: TypeId) -> Result<(), &'static str> {
        match self.module.types[ty.index()].clone {
            Some(_) => {
                Err("a twin clone or clone_in of a core type with a clone body is not built")
            }
            None => Ok(()),
        }
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
    /// `$clone_twin`: the own twin `dst` at `to` of the stable `src` at `from`, field by
    /// field (a twin's field k pairs with field k). Text and spans become blocks of
    /// capacity equal to their length, a stable field a `Box` node or the inline twin. A
    /// refusal drops what was written in reverse, as `$clone` does.
    pub(super) fn clone_twin_function(
        &mut self,
        c: &mut Code,
        src: TypeId,
        dst: TypeId,
        from: ValueId,
        to: ValueId,
    ) -> Result<(), &'static str> {
        self.walkable(src)?;
        self.walkable(dst)?;
        self.structural_clone_allowed(src, false)?;
        self.structural_clone_allowed(dst, false)?;
        let fail = c.block(&[Ty::Scalar(Scalar::U64); 2]);
        let (xs, ys) = (self.offsets(src), self.offsets(dst));
        match (
            self.module.types[src.index()].body.clone(),
            self.module.types[dst.index()].body.clone(),
        ) {
            (ShapeBody::Record(a), ShapeBody::Record(b)) if a.len() == b.len() => {
                let a: Vec<_> = a.into_iter().map(|f| f.ty).collect();
                let b: Vec<_> = b.into_iter().map(|f| f.ty).collect();
                self.twin_fields(c, (&a, &b), (&xs[0], &ys[0]), from, to, fail)?;
            }
            (ShapeBody::Variant(a), ShapeBody::Variant(b)) if a.len() == b.len() => {
                let at = c.address(from, PlaceTy::Declared(src));
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address: at,
                });
                let done = c.block(&[]);
                let mut arms = vec![];
                for i in 0..a.len() {
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
                for (i, (ca, cb)) in a.into_iter().zip(b).enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    if ca.fields.len() != cb.fields.len() {
                        return Err("these clone twins are not built");
                    }
                    let y = c.address(to, PlaceTy::Declared(dst));
                    c.inst(Inst::SetCase {
                        address: y,
                        case: i as u32,
                    });
                    let a: Vec<_> = ca.fields.into_iter().map(|f| f.ty).collect();
                    let b: Vec<_> = cb.fields.into_iter().map(|f| f.ty).collect();
                    self.twin_fields(c, (&a, &b), (&xs[i], &ys[i]), from, to, fail)?;
                    c.jump(done, &[]);
                }
                c.start(done);
            }
            _ => return Err("these clone twins are not built"),
        }
        let zero = c.constant(0);
        ret(c, &[zero, zero]);
        c.start(fail);
        let [requested, available] = c.params(fail);
        ret(c, &[requested, available]);
        Ok(())
    }
    /// The field pairs in order; a refusal at field k drops the own fields written
    /// before it, k-1 down to 0, before `fail`.
    fn twin_fields(
        &mut self,
        c: &mut Code,
        (a, b): (&[FieldTy], &[FieldTy]),
        (xs, ys): (&[u64], &[u64]),
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        let mut undo = fail;
        for (k, (fa, fb)) in a.iter().zip(b).enumerate() {
            let (x, y) = (c.offset(from, xs[k]), c.offset(to, ys[k]));
            self.twin_field(c, fa, fb, x, y, undo)?;
            if !self.owns(fb, &mut vec![]) {
                continue;
            }
            let next = c.block(&[Ty::Scalar(Scalar::U64); 2]);
            let here = detour(c);
            c.start(next);
            let [requested, available] = c.params(next);
            self.drop_field(c, fb, y)?;
            c.jump(undo, &[requested, available]);
            c.start(here);
            undo = next;
        }
        Ok(())
    }
    fn twin_field(
        &mut self,
        c: &mut Code,
        source: &FieldTy,
        target: &FieldTy,
        from: ValueId,
        to: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        match (source, target) {
            // Text or a FixedSpan of scalars: `W` count at the locator, then the bytes.
            (FieldTy::Locator(span), FieldTy::Seq(item)) => {
                match &**span {
                    FieldTy::Text => {}
                    FieldTy::FixedSpan(packed)
                        if matches!(**item, FieldTy::Scalar(_)) && packed == item => {}
                    _ => return Err("a clone of a span of non-scalar items is not built"),
                }
                let blob = c.read(from, Ty::Ptr);
                let at = c.offset(from, 8);
                let at = c.read(at, Ty::Scalar(Scalar::U64));
                let w = c.offset(blob, 8);
                let width = c.read(w, Ty::Scalar(Scalar::U64));
                let len = Derive::packed_count(c, blob, at, width);
                let start = c.binary(BinaryOp::Add, at, width);
                let data = c.offset_value(blob, start);
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
                let n = c.offset(to, 8);
                store(c, n, zero);
                let cap = c.offset(to, 16);
                store(c, cap, zero);
                c.jump(done, &[]);
                c.start(some);
                let stride = c.constant(self.size_of(item));
                let bytes = c.mul(len, stride);
                let charge = round8_value(c, bytes);
                let block = alloc_value(c, charge, bytes, fail);
                copy_value(c, block, data, bytes);
                store(c, to, block);
                let n = c.offset(to, 8);
                store(c, n, len);
                let cap = c.offset(to, 16);
                store(c, cap, len);
                c.jump(done, &[]);
                c.start(done);
            }
            // A stable field becomes a new node holding its twin.
            (FieldTy::Stable(t), FieldTy::Box(u)) => {
                let (t, u) = (TypeId(t.0), TypeId(u.0));
                let size = self.row_size(u);
                let block = alloc(c, AllocKind::Own, round8(size), size, fail);
                let f = self.function(t, KeyOp::CloneTwin(u.0))?;
                let old = c.read(from, Ty::Ptr);
                let x = c.address(old, PlaceTy::Declared(t));
                let y = c.address(block, PlaceTy::Declared(u));
                self.call_checked(c, f, x, y, fail, Some((block, round8(size))));
                store(c, to, block);
            }
            (FieldTy::Stable(t), FieldTy::Inline(u)) if t != u => {
                let (t, u) = (TypeId(t.0), TypeId(u.0));
                let f = self.function(t, KeyOp::CloneTwin(u.0))?;
                let old = c.read(from, Ty::Ptr);
                let x = c.address(old, PlaceTy::Declared(t));
                let y = c.address(to, PlaceTy::Declared(u));
                self.call_checked(c, f, x, y, fail, None);
            }
            // The same field type in both: bits, or a stable value kept in its region.
            (a, b) if a == b && !self.owns(b, &mut vec![]) => {
                let size = self.size_of(b);
                copy(c, to, from, size);
            }
            _ => return Err("this field of a clone into the twin type is not built"),
        }
        Ok(())
    }
    /// `$clone_in`: a new block of the stable row `dst` admitted in `region`, then its
    /// fields from the `src` value at `from` in order (a twin's field k pairs with field
    /// k). A refusal returns a null block; what was admitted stays charged to the region
    /// until it closes (T1.5), so nothing is dropped.
    pub(super) fn clone_in_function(
        &mut self,
        c: &mut Code,
        src: TypeId,
        dst: TypeId,
        from: ValueId,
        region: ValueId,
    ) -> Result<(), &'static str> {
        self.structural_clone_allowed(src, false)?;
        self.structural_clone_allowed(dst, false)?;
        let fail = c.block(&[Ty::Scalar(Scalar::U64); 2]);
        let block = self.clone_in_block(c, src, dst, from, region, fail)?;
        let zero = c.constant(0);
        ret(c, &[block, zero, zero]);
        c.start(fail);
        let [requested, available] = c.params(fail);
        let null = c.value(Ty::Ptr);
        c.inst(Inst::Const {
            result: null,
            bits: 0,
        });
        ret(c, &[null, requested, available]);
        Ok(())
    }
    fn clone_in_block(
        &mut self,
        c: &mut Code,
        src: TypeId,
        dst: TypeId,
        from: ValueId,
        region: ValueId,
        fail: BlockId,
    ) -> Result<ValueId, &'static str> {
        self.walkable(src)?;
        self.walkable(dst)?;
        let (Layout::Fixed(f), Some(logical)) = (
            &self.recipes[dst.index()].layout,
            self.recipes[dst.index()].logical_size,
        ) else {
            return Err("a clone_in into a compact type is not built");
        };
        let size = f.size;
        let charge = c.constant(round8(logical));
        let size = c.constant(size);
        let block = alloc_place(
            c,
            AllocKind::Plain,
            AllocPlace::Region(region),
            charge,
            size,
            fail,
        );
        let (xs, ys) = (self.offsets(src), self.offsets(dst));
        match (
            self.module.types[src.index()].body.clone(),
            self.module.types[dst.index()].body.clone(),
        ) {
            (ShapeBody::Record(a), ShapeBody::Record(b)) if a.len() == b.len() => {
                for (i, (fa, fb)) in a.iter().zip(&b).enumerate() {
                    let (x, y) = (c.offset(from, xs[0][i]), c.offset(block, ys[0][i]));
                    self.clone_in_field(c, &fa.ty, &fb.ty, x, y, region, fail)?;
                }
            }
            (ShapeBody::Variant(a), ShapeBody::Variant(b)) if a.len() == b.len() => {
                let at = c.address(from, PlaceTy::Declared(src));
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address: at,
                });
                let done = c.block(&[]);
                let mut arms = vec![];
                for i in 0..a.len() {
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
                for (i, (ca, cb)) in a.iter().zip(&b).enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    let to = c.address(block, PlaceTy::Declared(dst));
                    c.inst(Inst::SetCase {
                        address: to,
                        case: i as u32,
                    });
                    if ca.fields.len() != cb.fields.len() {
                        return Err("these clone_in twins are not built");
                    }
                    for (k, (fa, fb)) in ca.fields.iter().zip(&cb.fields).enumerate() {
                        let (x, y) = (c.offset(from, xs[i][k]), c.offset(block, ys[i][k]));
                        self.clone_in_field(c, &fa.ty, &fb.ty, x, y, region, fail)?;
                    }
                    c.jump(done, &[]);
                }
                c.start(done);
            }
            _ => return Err("these clone_in twins are not built"),
        }
        Ok(block)
    }
    /// Whether `field` is bits alone: no own part and nothing that names a block.
    fn bits(&self, field: &FieldTy, seen: &mut Vec<u32>) -> bool {
        match field {
            FieldTy::Scalar(_) | FieldTy::Fn => true,
            FieldTy::Array(item, _) => self.bits(item, seen),
            FieldTy::Inline(t) => {
                if seen.contains(&t.0) {
                    return true;
                }
                seen.push(t.0);
                match self.module.types[t.0 as usize].body.clone() {
                    ShapeBody::Record(fs) => fs.iter().all(|f| self.bits(&f.ty, seen)),
                    ShapeBody::Variant(cs) => cs
                        .iter()
                        .all(|c| c.fields.iter().all(|f| self.bits(&f.ty, seen))),
                }
            }
            _ => false,
        }
    }
    /// One field of a `clone_in`: `from` holds the source field, `to` the new block's.
    #[allow(clippy::too_many_arguments)]
    fn clone_in_field(
        &mut self,
        c: &mut Code,
        source: &FieldTy,
        target: &FieldTy,
        from: ValueId,
        to: ValueId,
        region: ValueId,
        fail: BlockId,
    ) -> Result<(), &'static str> {
        match (source, target) {
            // A stable field holds its value's block: copy the value into its own one.
            (_, FieldTy::Stable(u)) => {
                let (src, at) = match source {
                    FieldTy::Inline(t) => (TypeId(t.0), from),
                    FieldTy::Box(t) | FieldTy::Stable(t) => (TypeId(t.0), c.read(from, Ty::Ptr)),
                    _ => return Err("this field of a clone_in is not built"),
                };
                let f = self.function(src, KeyOp::CloneIn(u.0))?;
                let x = c.address(at, PlaceTy::Declared(src));
                let signature = self.module.functions[f.index()].signature;
                let block = c.value(Ty::Ptr);
                let requested = c.value(Ty::Scalar(Scalar::U64));
                let available = c.value(Ty::Scalar(Scalar::U64));
                let args = PoolRange::push(
                    &mut c.body.pools.args,
                    [Arg::Address(x), Arg::Value(region)],
                );
                let results =
                    PoolRange::push(&mut c.body.pools.values, [block, requested, available]);
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
                c.jump(fail, &[requested, available]);
                c.start(yes);
                store(c, to, block);
            }
            // A String becomes Text and a List a FixedSpan: a region blob `{_, W = 8 @8,
            // count @16, items @24}` charged round8(8 + len × size), located at 16.
            (FieldTy::Seq(item), FieldTy::Locator(span)) => {
                match &**span {
                    FieldTy::Text => {}
                    FieldTy::FixedSpan(packed)
                        if matches!(**item, FieldTy::Scalar(_)) && packed == item => {}
                    _ => return Err("a clone_in of a list of non-scalar items is not built"),
                }
                let es = c.constant(self.size_of(item));
                let len_at = c.offset(from, 8);
                let len = c.read(len_at, Ty::Scalar(Scalar::U64));
                let bytes = c.mul(len, es);
                let eight = c.constant(8);
                let counted = c.binary(BinaryOp::Add, bytes, eight);
                let charge = round8_value(c, counted);
                let head = c.constant(24);
                let size = c.binary(BinaryOp::Add, bytes, head);
                let blob = alloc_place(
                    c,
                    AllocKind::Plain,
                    AllocPlace::Region(region),
                    charge,
                    size,
                    fail,
                );
                let w = c.offset(blob, 8);
                store(c, w, eight);
                let count = c.offset(blob, 16);
                store(c, count, len);
                let zero = c.constant(0);
                let some = c.compare(CompareOp::Ne, len, zero);
                let (yes, done) = (c.block(&[]), c.block(&[]));
                c.branch(some, yes, done);
                c.start(yes);
                let items = c.offset(blob, 24);
                let old = c.read(from, Ty::Ptr);
                copy_value(c, items, old, bytes);
                c.jump(done, &[]);
                c.start(done);
                store(c, to, blob);
                let at = c.offset(to, 8);
                let sixteen = c.constant(16);
                store(c, at, sixteen);
            }
            (FieldTy::Locator(_), FieldTy::Locator(_)) => {
                return Err("a clone_in of stable Text or a span is not built");
            }
            (a, b) if a == b && self.bits(b, &mut vec![]) => {
                let size = self.size_of(b);
                copy(c, to, from, size);
            }
            _ => return Err("this field of a clone_in is not built"),
        }
        Ok(())
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
fn alloc_kind(
    c: &mut Code,
    kind: AllocKind,
    charge: ValueId,
    size: ValueId,
    fail: BlockId,
) -> ValueId {
    alloc_place(c, kind, AllocPlace::Current, charge, size, fail)
}
/// Admit `charge` at `place`; a refusal jumps to `fail(charge, available)`.
fn alloc_place(
    c: &mut Code,
    kind: AllocKind,
    place: AllocPlace,
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
        place,
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
    /// MEM-17's selection: the part of `x` (held at `v`) that is `u` or, when `twin`,
    /// `u`'s declared twin, through `Box`, `Rc` and `Arc` payloads outermost first. Its
    /// type and a pointer to it.
    fn clone_part(
        &mut self,
        x: TyId,
        u: TyId,
        v: ValueId,
        twin: bool,
        n: NodeId,
    ) -> Option<(TyId, ValueId)> {
        let mut part = self.bare(x);
        let twin = match self.t.kind(u) {
            TyKind::Adt(d, xs) if twin && xs.is_empty() => {
                crate::decls::twin_of(&self.p.decls, &self.t, *d)
            }
            _ => None,
        };
        let mut at = match self.rep(part).ok()? {
            Rep::Place(_) => v,
            _ => {
                let slot = self.slot(part, Some(v), n)?;
                self.pointer(slot, n)
            }
        };
        loop {
            if part == u {
                return Some((part, at));
            }
            let TyKind::Adt(d, xs) = self.t.kind(part).clone() else {
                break;
            };
            if Some(d) == twin && xs.is_empty() {
                return Some((part, at));
            }
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
        self.gap(n, "a clone into the twin type is not built");
        None
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
        let (part, at) = self.clone_part(x, u, v, true, n)?;
        // `src` is the row copied from, `id` the row of `u` written.
        let (src, id, function, at) = if part == u {
            let (id, function, _) = self.key_function(KeyOp::Clone, u, n)?;
            (id, id, function, at)
        } else {
            // The stable twin of an own `u` (MEM-17), held as its block's pointer.
            let Ok(Rep::Place(target)) = self.rep(u) else {
                self.gap(n, "a clone into the twin type is not built");
                return None;
            };
            let (src, function, _) = self.key_function(KeyOp::CloneTwin(target.0), part, n)?;
            if !self.e.by_block(src) || self.e.by_block(target) {
                self.gap(n, "a clone into the twin type is not built");
                return None;
            }
            let block = self.get_word(AddressBase::Value(at), 0, Ty::Ptr, n)?;
            (src, target, function, block)
        };
        let rid = self.declared(result)?;
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(rid), n);
        let from = self.address(AddressBase::Value(at), &[], PlaceTy::Declared(src), n);
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
    /// `clone_in(r, x)` expecting a stable `u` in `r` (MEM-17): the part of `x` that is
    /// `u` or its twin, copied by `$clone_in[part, u]` into a new block in `r` whose
    /// pointer is the `Ok` payload.
    pub(super) fn clone_in_value(
        &mut self,
        x: TyId,
        u: TyId,
        region: ValueId,
        v: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = self.bare(u);
        let stable = matches!(self.t.kind(u), TyKind::Adt(d, _)
            if self.p.decls[d.index()].repr == Repr::Stable && self.p.decls[d.index()].compact.is_none());
        if !stable {
            self.gap(n, "a clone_in into this type is not built");
            return None;
        }
        let (part, at) = self.clone_part(x, u, v, true, n)?;
        if has_param(&self.t, part) || has_param(&self.t, u) {
            self.gap(n, "a generic clone_in is not built");
            return None;
        }
        let Ok(Rep::Place(target)) = self.rep(u) else {
            self.gap(n, "a clone_in into this type is not built");
            return None;
        };
        let (id, function, _) = self.key_function(KeyOp::CloneIn(target.0), part, n)?;
        let rid = self.declared(result)?;
        if !self.live() {
            return None;
        }
        // A stable source is held as its block's pointer (MEM-5).
        let at = if self.e.by_block(id) {
            self.get_word(AddressBase::Value(at), 0, Ty::Ptr, n)?
        } else {
            at
        };
        let to = self.new_slot(PlaceTy::Declared(rid), n);
        let from = self.address(AddressBase::Value(at), &[], PlaceTy::Declared(id), n);
        let signature = self.e.module.functions[function.index()].signature;
        let block = self.value(Ty::Ptr);
        let requested = self.value(Ty::Scalar(Scalar::U64));
        let available = self.value(Ty::Scalar(Scalar::U64));
        let args = PoolRange::push(
            &mut self.b.pools.args,
            [Arg::Address(from), Arg::Value(region)],
        );
        let results = PoolRange::push(&mut self.b.pools.values, [block, requested, available]);
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
        let holder = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        self.inst(
            Inst::Store {
                address: holder,
                value: block,
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
            grant: 0,
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
            for op in [KeyOp::Clone, KeyOp::CloneIn(0), KeyOp::CloneTwin(0)] {
                let error =
                    super::super::key_glue::ensure(&mut base.clone(), TypeId(ty), op).unwrap_err();
                assert_eq!(
                    error,
                    "core-declared clone bodies for slot storage and core finalizers are not built"
                );
            }
        }
        super::super::key_glue::ensure(&mut base, TypeId(4), KeyOp::Clone).unwrap();
    }
}
