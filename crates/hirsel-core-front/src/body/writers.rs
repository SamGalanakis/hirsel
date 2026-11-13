//! MEM-4 streaming writers. `build_stream_in` admits one blob `{n @0, W @8, bytes @16}`
//! and keeps the build's state in a slot `{blob @0, used @8, end @16, W @24, failed @32}`:
//! `used` is the offset of the next byte, `end` is `16 + n`. A `Writer[U]` is a pointer
//! to its frame `{state @0, start @8, count @16, done @24}`: where its value starts, the
//! elements appended so far, and whether it completed. A `Text` or sequence writer
//! reserves its header (`W`, or `2W`) when it is made. A `Child[U]` is `{blob, start}`:
//! the destination's identity is its blob's address, and a child a callback returns is
//! accepted only from the completed frame handed to it. The first failed write marks
//! the build failed; every later write then fails, and so does the build, so a result
//! that is ignored never publishes a partly written value.
use super::*;
use hirsel_core_ir::RaiseCause;

const HEAD: u64 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Text,
    Seq,
    Indexed,
    Other,
}

/// The blocks a writer operation's failures go to, made when first used: `over` (no
/// parameters) writes `Error(Overflow)`; `failed` (a pointer to a `BuildError`) writes
/// that error.
#[derive(Default)]
struct Sinks {
    over: Option<BlockId>,
    failed: Option<BlockId>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn shape(&self, ty: TyId) -> Shape {
        match self.role_of(ty) {
            Some((Role::Text, _)) => Shape::Text,
            Some((Role::SeqSpan, _)) => Shape::Seq,
            Some((Role::IndexedSpan, _)) => Shape::Indexed,
            _ => Shape::Other,
        }
    }
    fn words(&mut self, k: u64, n: NodeId) -> Option<AddressId> {
        let ty = self.t.mk(TyKind::Array(scalar(Scalar::U64), k));
        let id = self.declared(ty)?;
        Some(self.new_slot(PlaceTy::Declared(id), n))
    }
    fn over(&mut self, sinks: &mut Sinks) -> BlockId {
        *sinks.over.get_or_insert_with(|| self.new_block(&[]))
    }
    fn failed(&mut self, sinks: &mut Sinks) -> BlockId {
        *sinks
            .failed
            .get_or_insert_with(|| self.new_block(&[Ty::Ptr]))
    }
    fn u64k(&mut self, k: u64, n: NodeId) -> Option<ValueId> {
        self.konst(Ty::Scalar(Scalar::U64), k, n)
    }
    fn state_word(&mut self, state: ValueId, offset: u64, t: Ty, n: NodeId) -> Option<ValueId> {
        self.get_word(AddressBase::Value(state), offset, t, n)
    }

    /// Reserve `k` bytes at the build's next byte: their offset, or a jump to `over`
    /// when they do not fit or the build already failed (MEM-4: checked before any
    /// byte is written).
    fn reserve(
        &mut self,
        state: ValueId,
        k: ValueId,
        sinks: &mut Sinks,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let over = self.over(sinks);
        let failed = self.state_word(state, 32, u64t, n)?;
        let zero = self.u64k(0, n)?;
        let bad = self.test(CompareOp::Ne, failed, zero, n);
        let go = self.new_block(&[]);
        self.branch(bad, over, go, n);
        self.start(go);
        let used = self.state_word(state, 8, u64t, n)?;
        let end = self.state_word(state, 16, u64t, n)?;
        let room = self.word_op(BinaryOp::Sub, end, used, n);
        let big = self.test(CompareOp::Gt, k, room, n);
        let fits = self.new_block(&[]);
        self.branch(big, over, fits, n);
        self.start(fits);
        let next = self.add(used, k, n);
        self.put_word(AddressBase::Value(state), &[], 8, next, n)?;
        Some(used)
    }

    /// Store the `W`-wide word `v` at `blob + at`.
    fn put_w(
        &mut self,
        blob: ValueId,
        at: ValueId,
        w: ValueId,
        v: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let done = self.new_block(&[]);
        for (bytes, s) in [(2, Scalar::U16), (4, Scalar::U32), (8, Scalar::U64)] {
            let next = if bytes != 8 {
                let k = self.u64k(bytes, n)?;
                let c = self.test(CompareOp::Eq, w, k, n);
                let (yes, no) = (self.new_block(&[]), self.new_block(&[]));
                self.branch(c, yes, no, n);
                self.start(yes);
                Some(no)
            } else {
                None
            };
            let x = if s == Scalar::U64 {
                v
            } else {
                self.narrow(v, s, n)
            };
            let a = self.address(
                AddressBase::Value(blob),
                &[Step::Byte(at)],
                PlaceTy::Value(Ty::Scalar(s)),
                n,
            );
            self.inst(
                Inst::Store {
                    address: a,
                    value: x,
                },
                n,
            );
            self.jump(done, &[], n);
            if let Some(no) = next {
                self.start(no);
            }
        }
        self.start(done);
        Some(())
    }
    fn narrow(&mut self, v: ValueId, s: Scalar, n: NodeId) -> ValueId {
        let r = self.value(Ty::Scalar(s));
        self.inst(
            Inst::Convert {
                result: r,
                flag: None,
                operand: v,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        r
    }
    fn store_at(&mut self, blob: ValueId, at: ValueId, v: ValueId, n: NodeId) {
        let t = self.b.values[v.index()];
        let a = self.address(
            AddressBase::Value(blob),
            &[Step::Byte(at)],
            PlaceTy::Value(t),
            n,
        );
        self.inst(
            Inst::Store {
                address: a,
                value: v,
            },
            n,
        );
    }

    /// A new writer for a `ty` at the build's next byte, its header reserved.
    fn new_frame(
        &mut self,
        state: ValueId,
        ty: TyId,
        sinks: &mut Sinks,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let w = self.state_word(state, 24, u64t, n)?;
        let start = match self.shape(ty) {
            Shape::Text => self.reserve(state, w, sinks, n)?,
            Shape::Seq | Shape::Indexed => {
                let two = self.add(w, w, n);
                self.reserve(state, two, sinks, n)?
            }
            Shape::Other => {
                // A writer that reserves nothing still fails once the build has.
                let zero = self.u64k(0, n)?;
                self.reserve(state, zero, sinks, n)?
            }
        };
        let f = self.words(4, n)?;
        let zero = self.u64k(0, n)?;
        self.put_word(AddressBase::Address(f), &[], 0, state, n)?;
        self.put_word(AddressBase::Address(f), &[], 8, start, n)?;
        self.put_word(AddressBase::Address(f), &[], 16, zero, n)?;
        self.put_word(AddressBase::Address(f), &[], 24, zero, n)?;
        Some(self.pointer(f, n))
    }

    /// The `Result[_, BuildError]` slot of `result` and the `BuildError` row.
    fn build_result(&mut self, result: TyId, n: NodeId) -> Option<(AddressId, TypeId)> {
        let id = self.declared(result)?;
        let error = self.p.core_decl("BuildError")?;
        let error = self.t.mk(TyKind::Adt(error, vec![]));
        let eid = self.declared(error)?;
        Some((self.new_slot(PlaceTy::Declared(id), n), eid))
    }
    fn error_place(&mut self, to: AddressId, eid: TypeId, n: NodeId) -> AddressId {
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(eid),
            n,
        )
    }
    /// `Ok(child)` of the frame `{blob, start}` into `to`.
    fn ok_child(&mut self, to: AddressId, blob: ValueId, start: ValueId, n: NodeId) -> Option<()> {
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let ok = [Step::CaseField { case: 0, field: 0 }];
        self.put_word(AddressBase::Address(to), &ok, 0, blob, n)?;
        self.put_word(AddressBase::Address(to), &ok, 8, start, n)
    }
    /// End an operation: its sinks write their errors into `to` and mark the build
    /// failed; everything continues at `join`.
    fn close_sinks(
        &mut self,
        sinks: Sinks,
        state: ValueId,
        to: AddressId,
        eid: TypeId,
        join: BlockId,
        n: NodeId,
    ) -> Option<()> {
        if let Some(over) = sinks.over {
            self.start(over);
            let k = self.u64k(1, n)?;
            self.put_word(AddressBase::Value(state), &[], 32, k, n)?;
            let e = self.error_place(to, eid, n);
            self.inst(
                Inst::SetCase {
                    address: e,
                    case: 1,
                },
                n,
            );
            self.jump(join, &[], n);
        }
        if let Some(failed) = sinks.failed {
            self.start(failed);
            let from = self.block_params(failed)[0];
            let k = self.u64k(1, n)?;
            self.put_word(AddressBase::Value(state), &[], 32, k, n)?;
            let e = self.error_place(to, eid, n);
            self.copy_to(e, from, eid, n);
            self.jump(join, &[], n);
        }
        self.start(join);
        Some(())
    }
    fn raise_bounds(&mut self, n: NodeId) {
        let cleanup = self.fail_to(n);
        self.end(
            Terminator::Raise {
                cause: RaiseCause::Bounds,
                cleanup,
            },
            Some(n),
        );
    }
    /// Go on only when `c` holds; otherwise fail with `Bounds`.
    fn or_bounds(&mut self, c: ValueId, n: NodeId) {
        let (yes, no) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(c, yes, no, n);
        self.start(no);
        self.raise_bounds(n);
        self.start(yes);
    }

    /// `scoped fn(take Writer[ty, in r]) -> Result[Child[ty], BuildError]`, with the
    /// writer declaration and region of `writer`, a writer type.
    fn writer_fn(&mut self, writer: TyId, ty: TyId) -> Option<TyId> {
        let TyKind::Adt(d, args) = self.t.kind(self.bare(writer)).clone() else {
            return None;
        };
        let mut args = args;
        args[0] = ty;
        let w = self.t.mk(TyKind::Adt(d, args));
        let child = self.p.core_decl("Child")?;
        let child = self.t.mk(TyKind::Adt(child, vec![ty]));
        let error = self.p.core_decl("BuildError")?;
        let error = self.t.mk(TyKind::Adt(error, vec![]));
        let result = self.p.core_decl("Result")?;
        let result = self.t.mk(TyKind::Adt(result, vec![child, error]));
        Some(self.t.mk(TyKind::Fn(
            vec![(Mode::Take, w)],
            result,
            false,
            Callable::Scoped,
        )))
    }

    /// Call `f` (of type `fty`) with a new writer for a `ty`; continue once it returned
    /// the child of that writer's completed value. Its error goes to `failed`.
    fn run_callback(
        &mut self,
        state: ValueId,
        ty: TyId,
        fty: TyId,
        f: ValueId,
        sinks: &mut Sinks,
        n: NodeId,
    ) -> Option<()> {
        let u64t = Ty::Scalar(Scalar::U64);
        let frame = self.new_frame(state, ty, sinks, n)?;
        let r = self.call_lowered(fty, f, &[Some(frame)], n)?;
        let TyKind::Fn(_, rt, _, _) = self.t.kind(fty).clone() else {
            return None;
        };
        let rid = self.declared(rt)?;
        let at = self.address(AddressBase::Value(r), &[], PlaceTy::Declared(rid), n);
        let case = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: case,
                address: at,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.test(CompareOp::Eq, case, zero, n);
        let (good, bad) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(ok, good, bad, n);
        self.start(bad);
        let error = self.p.core_decl("BuildError")?;
        let error = self.t.mk(TyKind::Adt(error, vec![]));
        let eid = self.declared(error)?;
        let e = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(eid),
            n,
        );
        let e = self.pointer(e, n);
        let failed = self.failed(sinks);
        self.jump(failed, &[e], n);
        self.start(good);
        let child = [Step::CaseField { case: 0, field: 0 }];
        let id = self.word_at(AddressBase::Address(at), &child, 0, Ty::Ptr, n)?;
        let id = self.read_bits(id, Ty::Ptr, n);
        let offset = self.word_at(AddressBase::Address(at), &child, 8, u64t, n)?;
        let offset = self.read_bits(offset, u64t, n);
        let blob = self.state_word(state, 0, Ty::Ptr, n)?;
        let start = self.get_word(AddressBase::Value(frame), 8, u64t, n)?;
        let done = self.get_word(AddressBase::Value(frame), 24, u64t, n)?;
        // MEM-4: the child must be the one of the frame handed out.
        let same = self.test(CompareOp::Eq, id, blob, n);
        self.or_bounds(same, n);
        let here = self.test(CompareOp::Eq, offset, start, n);
        self.or_bounds(here, n);
        let zero = self.u64k(0, n)?;
        let complete = self.test(CompareOp::Ne, done, zero, n);
        self.or_bounds(complete, n);
        Some(())
    }

    /// Complete the frame: its child `{blob, start}`.
    fn complete(&mut self, frame: ValueId, n: NodeId) -> Option<(ValueId, ValueId)> {
        let u64t = Ty::Scalar(Scalar::U64);
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let blob = self.state_word(state, 0, Ty::Ptr, n)?;
        let start = self.get_word(AddressBase::Value(frame), 8, u64t, n)?;
        let one = self.u64k(1, n)?;
        self.put_word(AddressBase::Value(frame), &[], 24, one, n)?;
        Some((blob, start))
    }

    /// `n` is `emit(f)`: the callback node.
    fn emit_callback(&self, a: NodeId) -> Option<NodeId> {
        if self.kind(a) != NodeKind::Call {
            return None;
        }
        let c = self.kids(a);
        if c.len() != 2 || self.kind(c[0]) != NodeKind::Path {
            return None;
        }
        match self.p.target(self.at, c[0]) {
            Some(Target::Definition(id)) => match self.p.by_definition[id.0 as usize].as_ref() {
                Some(Def::Fn(f)) if self.is_emit(*f as usize) => Some(c[1]),
                _ => None,
            },
            _ => None,
        }
    }
    pub(super) fn is_emit(&self, f: usize) -> bool {
        let g = &self.p.fns[f];
        g.body.is_none()
            && g.path.package_is("core", "1")
            && g.path.0.modules == ["core"]
            && g.path.0.name == "emit"
    }

    /// The case, fields and operands of the construction literal `a` of decl `d`.
    #[allow(clippy::type_complexity)]
    fn literal_parts(
        &self,
        a: NodeId,
        d: DeclId,
    ) -> Option<(
        Option<u32>,
        &'c [crate::decls::FieldDecl],
        bool,
        &'c [NodeId],
    )> {
        let p: &'c Program<'a> = self.p;
        let (callee, args): (NodeId, &'c [NodeId]) = match self.kind(a) {
            NodeKind::Call => {
                let c: &'c [NodeId] = self.kids(a);
                (c[0], &c[1..])
            }
            NodeKind::Path => (a, &[]),
            _ => return None,
        };
        if self.kind(callee) != NodeKind::Path {
            return None;
        }
        let decl = &p.decls[d.index()];
        match p.target(self.at, callee)? {
            Target::ContextualConstructor { name } => {
                let text = p.index.symbols.text(*name);
                match &decl.body {
                    DeclBody::Variant(cs) => {
                        let i = cs.iter().position(|c| c.name == text)?;
                        Some((Some(i as u32), &cs[i].fields, cs[i].labelled, args))
                    }
                    DeclBody::Record(fs) if decl.path.0.name == text => {
                        Some((None, fs, true, args))
                    }
                    _ => None,
                }
            }
            Target::Definition(id) => match (p.by_definition[id.0 as usize].as_ref(), &decl.body) {
                (Some(Def::Type(x)), DeclBody::Record(fs)) if *x == d && !args.is_empty() => {
                    Some((None, fs, true, args))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Write a `ty` from operand `a` at the build's next byte (MEM-4): `emit(f)`, a
    /// construction literal of a compact type, a view of a `String` for a `Text`, or a
    /// value of a plain type. `writer` is the writer type the build's writers share.
    fn write_operand(
        &mut self,
        state: ValueId,
        writer: TyId,
        ty: TyId,
        a: NodeId,
        sinks: &mut Sinks,
    ) -> Option<()> {
        // A field's anchor to its holder's region (MEM-6) is not part of its writer.
        let ty = self.bare(ty);
        if let Some(f) = self.emit_callback(a) {
            let fty = self.writer_fn(writer, ty)?;
            let (_, fv) = self.expr(
                f,
                Some(Want {
                    ty: fty,
                    view: false,
                    payload: 0,
                }),
            );
            return self.run_callback(state, ty, fty, fv?, sinks, a);
        }
        if let Some((d, _)) = self.compact_adt(ty) {
            let Some((case, fields, labelled, args)) = self.literal_parts(a, d) else {
                let _ = self.expr(a, None);
                self.refuse(
                    "MEM-4",
                    a,
                    "an emitted compact value is a construction literal of its type",
                );
                return None;
            };
            let k = fields.len();
            if args.len() != k {
                let at = args.get(k).copied().unwrap_or(a);
                self.refuse("LANG-11", at, format!("expected {k} arguments"));
                return None;
            }
            let ftys = self.compact_fields(ty, case)?;
            if let Some(c) = case {
                let s = Self::tag_scalar(self.variant_cases(ty)?);
                let size = self.u64k(s.size(), a)?;
                let at = self.reserve(state, size, sinks, a)?;
                let tag = self.konst(Ty::Scalar(s), u64::from(c), a)?;
                let blob = self.state_word(state, 0, Ty::Ptr, a)?;
                self.store_at(blob, at, tag, a);
            }
            for (i, (&e, f)) in args.iter().zip(fields).enumerate() {
                let e = match (self.kind(e), labelled) {
                    (NodeKind::LabelledArgument, true) if self.token_text(e) == f.name => {
                        self.kids(e)[0]
                    }
                    (NodeKind::LabelledArgument, true) => {
                        let name = &f.name;
                        self.refuse("LANG-9", e, format!("expected the field `{name}` here"));
                        return None;
                    }
                    (NodeKind::LabelledArgument, false) => {
                        self.refuse("LANG-9", e, "this payload is not labelled");
                        return None;
                    }
                    (_, true) => {
                        let name = &f.name;
                        self.refuse("LANG-9", e, format!("the field `{name}` is labelled"));
                        return None;
                    }
                    (_, false) => e,
                };
                self.write_operand(state, writer, ftys[i], e, sinks)?;
            }
            return Some(());
        }
        match self.shape(ty) {
            Shape::Text => {
                let s = self.string_view(a)?;
                let w = self.state_word(state, 24, Ty::Scalar(Scalar::U64), a)?;
                let len = self.get_word(AddressBase::Value(s), 8, Ty::Scalar(Scalar::U64), a)?;
                let k = self.add(w, len, a);
                let at = self.reserve(state, k, sinks, a)?;
                let blob = self.state_word(state, 0, Ty::Ptr, a)?;
                self.put_w(blob, at, w, len, a)?;
                let data = self.add(at, w, a);
                self.copy_string(blob, data, s, len, a)
            }
            Shape::Seq | Shape::Indexed => {
                let _ = self.expr(a, None);
                self.refuse("MEM-4", a, "a sequence is written through `emit(f)`");
                None
            }
            Shape::Other => {
                let (_, v) = self.expr(a, expect(ty));
                let v = v?;
                if let Some(size) = self.packed_plain_size(ty) {
                    let k = self.u64k(size, a)?;
                    let at = self.reserve(state, k, sinks, a)?;
                    let blob = self.state_word(state, 0, Ty::Ptr, a)?;
                    let r = self.rep(ty).ok()?;
                    return self.encode_plain(blob, at, ty, r, v, a);
                }
                self.gap(a, "this field of an emitted literal is not built");
                None
            }
        }
    }
    /// Copy the `len` bytes of the `String` that `s` points at to `blob + at`.
    fn copy_string(
        &mut self,
        blob: ValueId,
        at: ValueId,
        s: ValueId,
        len: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let zero = self.u64k(0, n)?;
        let some = self.test(CompareOp::Ne, len, zero, n);
        let (copy, after) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(some, copy, after, n);
        self.start(copy);
        let data = self.get_word(AddressBase::Value(s), 0, Ty::Ptr, n)?;
        let u8t = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let to = self.address(AddressBase::Value(blob), &[Step::Byte(at)], u8t, n);
        let from = self.address(AddressBase::Value(data), &[], u8t, n);
        self.inst(
            Inst::Copy {
                to,
                from,
                bytes: len,
            },
            n,
        );
        self.jump(after, &[], n);
        self.start(after);
        Some(())
    }

    /// The built-in form `emit(take w, x)` (MEM-4).
    pub(super) fn emit_form(
        &mut self,
        n: NodeId,
        f: usize,
        args: &[NodeId],
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let g = &self.p.fns[f];
        if args.len() == 1 {
            let _ = self.expr(args[0], None);
            self.refuse(
                "MEM-4",
                n,
                "`emit(f)` is written only as a field operand of an emitted literal",
            );
            return (ERROR, None);
        }
        if args.len() != 2 {
            self.refuse("LANG-11", n, "expected 2 arguments");
            return (ERROR, None);
        }
        let mut found = vec![None; g.generics.len()];
        if let Some(w) = want
            && let Some(result) = self.expected_part(g.result, w)
        {
            self.unify(result, w.ty, &mut found);
        }
        self.call_seq += 1;
        let call = self.call_seq;
        let mut writer = None;
        let pt = g.params[0].ty;
        if !self.argument(Mode::Take, pt, call, 0, args[0], &mut found, &mut writer) {
            let (t, v) = self.expr(args[0], None);
            self.arg_ty(pt, t, args[0], &mut found);
            writer = v;
        }
        let Some(ty) = found[0].filter(|&t| t != ERROR) else {
            if !self.broken {
                self.refuse("LANG-12", n, "a type parameter of the callee is still open");
            }
            return (ERROR, None);
        };
        let tys: Vec<TyId> = found.iter().map(|t| t.unwrap_or(ERROR)).collect();
        let result = subst(&mut self.t, g.result, &tys);
        let wty = subst(&mut self.t, pt, &tys);
        let v = self.emit_value(writer, wty, ty, args[1], result, n);
        (result, v)
    }
    fn emit_value(
        &mut self,
        writer: Option<ValueId>,
        wty: TyId,
        ty: TyId,
        x: NodeId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let shape = self.shape(ty);
        if matches!(shape, Shape::Seq | Shape::Indexed)
            || self.role_of(ty).is_some_and(|r| r.0 == Role::FixedSpan)
        {
            let _ = self.expr(x, None);
            self.gap(n, "emitting a whole sequence is not built");
            return None;
        }
        let frame = writer?;
        if !self.live() {
            let _ = self.expr(x, None);
            return None;
        }
        let (to, eid) = self.build_result(result, n)?;
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let mut sinks = Sinks::default();
        let join = self.new_block(&[]);
        if shape == Shape::Text {
            // The header was reserved with the writer; the bytes follow it.
            let s = self.string_view(x)?;
            self.push_bytes(state, s, &mut sinks, n)?;
            self.close_text(frame, n)?;
        } else {
            self.write_operand(state, wty, ty, x, &mut sinks)?;
        }
        let (blob, start) = self.complete(frame, n)?;
        self.ok_child(to, blob, start, n)?;
        self.jump(join, &[], n);
        self.close_sinks(sinks, state, to, eid, join, n)?;
        Some(self.pointer(to, n))
    }
    /// A pointer to the `String` a `Text` is written from: a place is viewed for the
    /// copy, never moved (OWN-7); any other operand is a `String` value.
    fn string_view(&mut self, a: NodeId) -> Option<ValueId> {
        let string = self.p.core_decl("String")?;
        let string = self.t.mk(TyKind::Adt(string, vec![]));
        let (t, v) = self.operand(a, expect(string));
        self.coerce(
            t,
            Want {
                ty: string,
                view: false,
                payload: 0,
            },
            a,
        );
        v
    }
    fn push_bytes(
        &mut self,
        state: ValueId,
        s: ValueId,
        sinks: &mut Sinks,
        n: NodeId,
    ) -> Option<()> {
        let len = self.get_word(AddressBase::Value(s), 8, Ty::Scalar(Scalar::U64), n)?;
        let at = self.reserve(state, len, sinks, n)?;
        let blob = self.state_word(state, 0, Ty::Ptr, n)?;
        self.copy_string(blob, at, s, len, n)
    }
    /// Write a `Text` frame's byte count into its reserved header.
    fn close_text(&mut self, frame: ValueId, n: NodeId) -> Option<()> {
        let u64t = Ty::Scalar(Scalar::U64);
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let (blob, used) = (
            self.state_word(state, 0, Ty::Ptr, n)?,
            self.state_word(state, 8, u64t, n)?,
        );
        let w = self.state_word(state, 24, u64t, n)?;
        let start = self.get_word(AddressBase::Value(frame), 8, u64t, n)?;
        let body = self.add(start, w, n);
        let len = self.word_op(BinaryOp::Sub, used, body, n);
        self.put_w(blob, start, w, len, n)
    }

    /// `writer.*`, `text.*`, `build_stream_in`, `finish` and `finish_trim`.
    pub(super) fn writer_op(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        if tys.contains(&ERROR) || !self.live() {
            return None;
        }
        let g = &self.p.fns[f];
        let module = g.path.0.modules[0].as_str();
        let name = g.path.0.name.as_str();
        let result = subst(&mut self.t, g.result, tys);
        match (module, name) {
            ("core", "build_stream_in") => {
                let fty = subst(&mut self.t, g.params[2].ty, tys);
                self.build_stream_in(tys[0], vals[0]?, vals[1]?, fty, vals[2]?, result, n)
            }
            ("core", "finish") => {
                let blob = self.get_word(AddressBase::Value(vals[0]?), 0, Ty::Ptr, n)?;
                let root =
                    self.get_word(AddressBase::Value(vals[0]?), 8, Ty::Scalar(Scalar::U64), n)?;
                let to = self.new_locator(tys[0], blob, root, n)?;
                Some(self.pointer(to, n))
            }
            ("core", "finish_trim") => {
                self.gap(
                    n,
                    "finish_trim needs a region refund instruction (FOR FORM)",
                );
                None
            }
            ("writer", "append") => {
                let shape = self.shape(tys[0]);
                let elem = self.role_of(tys[0]).and_then(|r| r.1.first().copied());
                if !matches!(shape, Shape::Seq | Shape::Indexed) || elem != Some(tys[1]) {
                    self.refuse(
                        "MEM-4",
                        n,
                        "`writer.append` writes the next element of a sequence",
                    );
                    return None;
                }
                let frame = self.read_writer(vals[0]?, n)?;
                let fty = subst(&mut self.t, g.params[1].ty, tys);
                self.append(frame, tys[1], fty, vals[1]?, result, n)
            }
            ("writer", "scalar") => {
                let frame = self.read_writer(vals[0]?, n)?;
                self.writer_scalar(frame, vals[1]?, result, n)
            }
            ("writer", "push") => {
                let frame = self.read_writer(vals[0]?, n)?;
                let (to, eid) = self.build_result(result, n)?;
                let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
                let mut sinks = Sinks::default();
                let join = self.new_block(&[]);
                self.push_bytes(state, vals[1]?, &mut sinks, n)?;
                self.inst(
                    Inst::SetCase {
                        address: to,
                        case: 0,
                    },
                    n,
                );
                self.jump(join, &[], n);
                self.close_sinks(sinks, state, to, eid, join, n)?;
                Some(self.pointer(to, n))
            }
            ("writer", "close") => self.close(tys[0], vals[0]?, result, n),
            ("writer", _) => {
                self.gap(n, "views of completed elements in a writer are not built");
                None
            }
            ("text", "len") => {
                let (blob, at, w) = self.locator(vals[0]?, n)?;
                self.word_in(blob, at, w, n)
            }
            ("text", "as_string") => self.as_string(vals[0]?, n),
            ("text", "copy_in") => self.copy_in(vals[0]?, vals[1]?, result, n),
            _ => {
                self.gap(n, "this writer operation is not built");
                None
            }
        }
    }
    /// The frame a `mut` writer argument's place holds.
    fn read_writer(&mut self, place: ValueId, n: NodeId) -> Option<ValueId> {
        self.get_word(AddressBase::Value(place), 0, Ty::Ptr, n)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_stream_in(
        &mut self,
        ty: TyId,
        region: ValueId,
        bytes: ValueId,
        fty: TyId,
        f: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let w: u64 = match (self.compact_adt(ty), self.shape(ty)) {
            (Some((d, _)), _) => match self.p.decls[d.index()].compact? {
                hirsel_core_layout::Width::U16 => 2,
                hirsel_core_layout::Width::U32 => 4,
                hirsel_core_layout::Width::U64 => 8,
            },
            (None, Shape::Text | Shape::Seq | Shape::Indexed) => 8,
            _ => {
                self.refuse(
                    "MEM-4",
                    n,
                    "a stream's root is a compact type, `Text` or a sequence",
                );
                return None;
            }
        };
        let (to, eid) = self.build_result(result, n)?;
        let join = self.new_block(&[]);
        let mut sinks = Sinks::default();
        // The blob cap of `W` (MEM-9), checked before anything is admitted.
        let cap = if w == 8 { u64::MAX } else { (1 << (8 * w)) - 1 };
        let cap = self.u64k(cap, n)?;
        let big = self.test(CompareOp::Gt, bytes, cap, n);
        let (capped, admit) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(big, capped, admit, n);
        self.start(capped);
        let e = self.error_place(to, eid, n);
        self.inst(
            Inst::SetCase {
                address: e,
                case: 1,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(admit);
        // Charge round8(n) (MEM-24); the blob is n bytes after its 16-byte head. A size
        // past u64 is flagged as the charge's overflow, so the admission is refused.
        let sized = self.new_block(&[u64t, u64t]);
        let limit = self.u64k(u64::MAX - HEAD - 7, n)?;
        let huge = self.test(CompareOp::Gt, bytes, limit, n);
        let (sat, fits) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(huge, sat, fits, n);
        self.start(sat);
        let max = self.u64k(u64::MAX, n)?;
        self.jump(sized, &[max, max], n);
        self.start(fits);
        let seven = self.u64k(7, n)?;
        let up = self.add(bytes, seven, n);
        let mask = self.u64k(!7, n)?;
        let charge = self.word_op(BinaryOp::BitAnd, up, mask, n);
        let size = self.add_k(bytes, HEAD, n)?;
        self.jump(sized, &[charge, size], n);
        self.start(sized);
        let p = self.block_params(sized);
        let (charge, size) = (p[0], p[1]);
        let blob = self.value(Ty::Ptr);
        let available = self.value(u64t);
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: blob,
                available,
                region_entry: None,
                kind: AllocKind::Plain,
                place: AllocPlace::Region(region),
                charge,
                overflow: Some(huge),
                size,
                align: 8,
                fail,
            },
            n,
        );
        let null = self.konst(Ty::Ptr, 0, n)?;
        let admitted = self.test(CompareOp::Ne, blob, null, n);
        let (yes, refused) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(admitted, yes, refused, n);
        self.start(refused);
        let e = self.error_place(to, eid, n);
        self.inst(
            Inst::SetCase {
                address: e,
                case: 0,
            },
            n,
        );
        self.capacity_at(
            AddressBase::Address(e),
            &[Step::CaseField { case: 0, field: 0 }],
            charge,
            available,
            n,
        )?;
        self.jump(join, &[], n);
        self.start(yes);
        let wv = self.u64k(w, n)?;
        let head = self.u64k(HEAD, n)?;
        self.put_word(AddressBase::Value(blob), &[], 0, bytes, n)?;
        self.put_word(AddressBase::Value(blob), &[], 8, wv, n)?;
        let s = self.words(5, n)?;
        let end = self.add(bytes, head, n);
        let zero = self.u64k(0, n)?;
        self.put_word(AddressBase::Address(s), &[], 0, blob, n)?;
        self.put_word(AddressBase::Address(s), &[], 8, head, n)?;
        self.put_word(AddressBase::Address(s), &[], 16, end, n)?;
        self.put_word(AddressBase::Address(s), &[], 24, wv, n)?;
        self.put_word(AddressBase::Address(s), &[], 32, zero, n)?;
        let state = self.pointer(s, n);
        self.run_callback(state, ty, fty, f, &mut sinks, n)?;
        // A build whose writes failed publishes nothing, whatever its callback returned.
        let failed = self.state_word(state, 32, u64t, n)?;
        let bad = self.test(CompareOp::Ne, failed, zero, n);
        let over = self.over(&mut sinks);
        let good = self.new_block(&[]);
        self.branch(bad, over, good, n);
        self.start(good);
        // The root is where the callback's frame started: the first byte after the head.
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let ok = [Step::CaseField { case: 0, field: 0 }];
        self.put_word(AddressBase::Address(to), &ok, 0, blob, n)?;
        self.put_word(AddressBase::Address(to), &ok, 8, head, n)?;
        self.jump(join, &[], n);
        self.close_build_sinks(sinks, state, to, eid, join, n)
    }
    /// `close_sinks` for `build_stream_in`, whose state exists only after admission.
    fn close_build_sinks(
        &mut self,
        sinks: Sinks,
        state: ValueId,
        to: AddressId,
        eid: TypeId,
        join: BlockId,
        n: NodeId,
    ) -> Option<ValueId> {
        self.close_sinks(sinks, state, to, eid, join, n)?;
        Some(self.pointer(to, n))
    }

    fn append(
        &mut self,
        frame: ValueId,
        elem: TyId,
        fty: TyId,
        f: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let (to, eid) = self.build_result(result, n)?;
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let mut sinks = Sinks::default();
        let join = self.new_block(&[]);
        self.run_callback(state, elem, fty, f, &mut sinks, n)?;
        let count = self.get_word(AddressBase::Value(frame), 16, u64t, n)?;
        let next = self.add_k(count, 1, n)?;
        self.put_word(AddressBase::Value(frame), &[], 16, next, n)?;
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.close_sinks(sinks, state, to, eid, join, n)?;
        Some(self.pointer(to, n))
    }

    /// `writer.scalar`: `c` in UTF-8 after one fit check.
    fn writer_scalar(
        &mut self,
        frame: ValueId,
        c: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u32t = Ty::Scalar(Scalar::U32);
        let (to, eid) = self.build_result(result, n)?;
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let mut sinks = Sinks::default();
        let join = self.new_block(&[]);
        // MEM-9 fixes a char's encoding as its scalar value in a u32; FORM has no
        // conversion from `char`, so the value is read back through memory.
        let slot = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::Char)), n);
        self.inst(
            Inst::Store {
                address: slot,
                value: c,
            },
            n,
        );
        let p = self.pointer(slot, n);
        let as_u32 = self.address(AddressBase::Value(p), &[], PlaceTy::Value(u32t), n);
        let u = self.read_bits(as_u32, u32t, n);
        let done = self.new_block(&[]);
        let limits = [(0x80u64, 1u64), (0x800, 2), (0x1_0000, 3)];
        for len in 1..=4u64 {
            let next = if len < 4 {
                let k = self.konst(u32t, limits[len as usize - 1].0, n)?;
                let small = self.test(CompareOp::Lt, u, k, n);
                let (yes, no) = (self.new_block(&[]), self.new_block(&[]));
                self.branch(small, yes, no, n);
                self.start(yes);
                Some(no)
            } else {
                None
            };
            let k = self.u64k(len, n)?;
            let at = self.reserve(state, k, &mut sinks, n)?;
            let blob = self.state_word(state, 0, Ty::Ptr, n)?;
            let lead: u64 = [0, 0, 0xC0, 0xE0, 0xF0][len as usize];
            for i in 0..len {
                let shift = 6 * (len - 1 - i);
                let shifted = if shift == 0 {
                    u
                } else {
                    let s = self.konst(u32t, shift, n)?;
                    self.u32_op(BinaryOp::Shr, u, s, n)
                };
                let (mask, mark) = if i == 0 {
                    (if len == 1 { 0x7F } else { 0x3F >> (len - 2) }, lead)
                } else {
                    (0x3F, 0x80)
                };
                let m = self.konst(u32t, mask, n)?;
                let bits = self.u32_op(BinaryOp::BitAnd, shifted, m, n);
                let mk = self.konst(u32t, mark, n)?;
                let byte = self.u32_op(BinaryOp::BitOr, bits, mk, n);
                let byte = self.narrow(byte, Scalar::U8, n);
                let off = self.add_k(at, i, n)?;
                self.store_at(blob, off, byte, n);
            }
            self.jump(done, &[], n);
            if let Some(no) = next {
                self.start(no);
            }
        }
        self.start(done);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.close_sinks(sinks, state, to, eid, join, n)?;
        Some(self.pointer(to, n))
    }
    fn u32_op(&mut self, op: BinaryOp, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }

    /// `writer.close`: a `Text`'s count, or a sequence's count and byte length, into
    /// the header reserved with the writer; an `IndexedSpan`'s table, checked whole,
    /// then written in one walk of the elements.
    fn close(&mut self, ty: TyId, frame: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let shape = self.shape(ty);
        if shape == Shape::Other {
            self.refuse(
                "MEM-4",
                n,
                "`writer.close` completes a `Text` or a sequence",
            );
            return None;
        }
        let (to, eid) = self.build_result(result, n)?;
        let state = self.get_word(AddressBase::Value(frame), 0, Ty::Ptr, n)?;
        let mut sinks = Sinks::default();
        let join = self.new_block(&[]);
        // A failed build completes nothing.
        let zero = self.u64k(0, n)?;
        let _ = self.reserve(state, zero, &mut sinks, n)?;
        if shape == Shape::Text {
            self.close_text(frame, n)?;
        } else {
            let blob = self.state_word(state, 0, Ty::Ptr, n)?;
            let w = self.state_word(state, 24, u64t, n)?;
            let used = self.state_word(state, 8, u64t, n)?;
            let start = self.get_word(AddressBase::Value(frame), 8, u64t, n)?;
            let count = self.get_word(AddressBase::Value(frame), 16, u64t, n)?;
            let second = self.add(start, w, n);
            let data = self.add(second, w, n);
            let len = self.word_op(BinaryOp::Sub, used, data, n);
            if shape == Shape::Indexed {
                let k = self.mul(count, w, n);
                let table = self.reserve(state, k, &mut sinks, n)?;
                let elem = self.role_of(ty)?.1[0];
                let lp = self.new_block(&[u64t, u64t]);
                self.jump(lp, &[zero, data], n);
                self.start(lp);
                let p = self.block_params(lp);
                let (i, at) = (p[0], p[1]);
                let more = self.test(CompareOp::Lt, i, count, n);
                let (body, after) = (self.new_block(&[]), self.new_block(&[]));
                self.branch(more, body, after, n);
                self.start(body);
                let iw = self.mul(i, w, n);
                let slot = self.add(table, iw, n);
                let off = self.word_op(BinaryOp::Sub, at, data, n);
                self.put_w(blob, slot, w, off, n)?;
                let next = self.skip(blob, at, w, elem, n)?;
                let i1 = self.add_k(i, 1, n)?;
                self.jump(lp, &[i1, next], n);
                self.start(after);
            }
            self.put_w(blob, start, w, count, n)?;
            self.put_w(blob, second, w, len, n)?;
        }
        let (blob, start) = self.complete(frame, n)?;
        self.ok_child(to, blob, start, n)?;
        self.jump(join, &[], n);
        self.close_sinks(sinks, state, to, eid, join, n)?;
        Some(self.pointer(to, n))
    }

    /// `text.as_string`: a `String` header viewing the text's bytes, which owns
    /// nothing (capacity 0). It lives in this frame, so a function whose result could
    /// carry it out is not built (Q38).
    fn as_string(&mut self, header: ValueId, n: NodeId) -> Option<ValueId> {
        if self.view_of_string_escapes() {
            self.gap(
                n,
                "a String view of a Text returned from its function is not built (Q38)",
            );
            return None;
        }
        let (blob, at, w) = self.locator(header, n)?;
        let len = self.word_in(blob, at, w, n)?;
        let data = self.add(at, w, n);
        let bytes = self.address(
            AddressBase::Value(blob),
            &[Step::Byte(data)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let data = self.pointer(bytes, n);
        let string = self.p.core_decl("String")?;
        let string = self.t.mk(TyKind::Adt(string, vec![]));
        let id = self.declared(string)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let zero = self.u64k(0, n)?;
        self.put_word(AddressBase::Address(to), &[], 0, data, n)?;
        self.put_word(AddressBase::Address(to), &[], 8, len, n)?;
        self.put_word(AddressBase::Address(to), &[], 16, zero, n)?;
        Some(self.pointer(to, n))
    }
    fn view_of_string_escapes(&mut self) -> bool {
        let Some(string) = self.p.core_decl("String") else {
            return true;
        };
        let string = self.t.mk(TyKind::Adt(string, vec![]));
        let view = self.t.mk(TyKind::View(string));
        self.mentions(self.fd.result, view)
    }
    /// Whether `ty` holds `x` anywhere.
    fn mentions(&self, ty: TyId, x: TyId) -> bool {
        if ty == x {
            return true;
        }
        match self.t.kind(ty) {
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&y| self.mentions(y, x)),
            TyKind::Array(y, _) | TyKind::View(y) | TyKind::Anchored(y, _) => self.mentions(*y, x),
            _ => false,
        }
    }

    /// `text.copy_in(r, s)`: `{_, W = 8 @8, count @16, bytes @24}`, charged
    /// `round8(8 + len)` (MEM-24), located at 16.
    fn copy_in(&mut self, region: ValueId, s: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let id = self.declared(result)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.get_word(AddressBase::Value(s), 8, u64t, n)?;
        let (blob, available, charge) = self.region_blob(region, len, 1, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let admitted = self.test(CompareOp::Ne, blob, null, n);
        let (yes, no, join) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(admitted, yes, no, n);
        self.start(yes);
        let w = self.u64k(8, n)?;
        self.put_word(AddressBase::Value(blob), &[], 8, w, n)?;
        self.put_word(AddressBase::Value(blob), &[], 16, len, n)?;
        let data = self.u64k(24, n)?;
        self.copy_string(blob, data, s, len, n)?;
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let ok = [Step::CaseField { case: 0, field: 0 }];
        let at = self.u64k(16, n)?;
        self.put_word(AddressBase::Address(to), &ok, 0, blob, n)?;
        self.put_word(AddressBase::Address(to), &ok, 8, at, n)?;
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
            charge,
            available,
            n,
        )?;
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }

    /// Blob, offset and W of element `index` of the packed sequence `header`
    /// (MEM-4): an `IndexedSpan` reads its offset table, a `SeqSpan` walks from the
    /// first element. `index` is below the count.
    pub(super) fn packed_element_offset(
        &mut self,
        header: ValueId,
        elem: TyId,
        index: ValueId,
        indexed: bool,
        n: NodeId,
    ) -> Option<(ValueId, ValueId, ValueId)> {
        let u64t = Ty::Scalar(Scalar::U64);
        let elem = self.bare(elem);
        let (blob, at, w) = self.locator(header, n)?;
        let second = self.add(at, w, n);
        let data = self.add(second, w, n);
        let found = if indexed {
            let len = self.word_in(blob, second, w, n)?;
            let table = self.add(data, len, n);
            let iw = self.mul(index, w, n);
            let slot = self.add(table, iw, n);
            let off = self.word_in(blob, slot, w, n)?;
            self.add(data, off, n)
        } else {
            let zero = self.u64k(0, n)?;
            let lp = self.new_block(&[u64t, u64t]);
            self.jump(lp, &[zero, data], n);
            self.start(lp);
            let p = self.block_params(lp);
            let (i, el) = (p[0], p[1]);
            let more = self.test(CompareOp::Lt, i, index, n);
            let (body, after) = (self.new_block(&[]), self.new_block(&[]));
            self.branch(more, body, after, n);
            self.start(body);
            let next = self.skip(blob, el, w, elem, n)?;
            let i1 = self.add_k(i, 1, n)?;
            self.jump(lp, &[i1, next], n);
            self.start(after);
            el
        };
        Some((blob, found, w))
    }
    /// `seq.nth` and `indexed.get` of a locator element: `Some` of its locator below
    /// the count, else `None`.
    pub(super) fn packed_get(
        &mut self,
        header: ValueId,
        index: ValueId,
        elem: TyId,
        indexed: bool,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (_, _, _, count) = self.span_parts(header, n)?;
        let rep = self.rep(result).ok()?;
        let to = self.new_slot(rep.stored()?, n);
        let more = self.test(CompareOp::Lt, index, count, n);
        let (load, empty, done) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(more, load, empty, n);
        self.start(load);
        let (blob, off, w) = self.packed_element_offset(header, elem, index, indexed, n)?;
        let el = self.compact_value_at(blob, off, w, elem, n)?;
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let er = self.rep(elem).ok()?;
        if let Some(place) = er.stored() {
            let dest = self.address(
                AddressBase::Address(to),
                &[Step::CaseField { case: 1, field: 0 }],
                place,
                n,
            );
            let value = match er {
                Rep::Scalar(t) => self.read_bits(el, t, n),
                _ => self.pointer(el, n),
            };
            self.install(dest, er, Some(value), n);
        }
        self.jump(done, &[], n);
        self.start(empty);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        Some(self.pointer(to, n))
    }
}
