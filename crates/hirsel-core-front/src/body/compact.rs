//! MEM-9..12 compact values, read through their locators. A compact value, a `Text`
//! and a stable sequence are each held as a locator `{blob @0, at @8}` (16 bytes): the
//! blob records its `W` in bytes as a word at byte 8, and `at` is the byte where the
//! value's encoding starts. A field is found by skipping the fields before it, at a cost
//! fixed by their types (MEM-12); a view of a compact value is its locator, copied.
use super::*;

impl Cx<'_, '_, '_> {
    /// `ty` without its views and anchors.
    pub(super) fn bare(&self, mut ty: TyId) -> TyId {
        while let TyKind::View(x) | TyKind::Anchored(x, _) = self.t.kind(ty) {
            ty = *x;
        }
        ty
    }
    /// The compact declaration encoding values of `ty` (MEM-9), and its arguments.
    pub(super) fn compact_adt(&self, ty: TyId) -> Option<(DeclId, Vec<TyId>)> {
        match self.t.kind(self.bare(ty)) {
            TyKind::Adt(d, args)
                if self.p.decls[d.index()].repr == Repr::Stable
                    && self.p.decls[d.index()].compact.is_some() =>
            {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }
    pub(super) fn role_of(&self, ty: TyId) -> Option<(Role, Vec<TyId>)> {
        match self.t.kind(self.bare(ty)) {
            TyKind::Adt(d, args) => Some((self.p.decls[d.index()].role?, args.clone())),
            _ => None,
        }
    }
    /// The byte width of the tag of a variant of `cases` cases (MEM-9).
    pub(super) fn tag_scalar(cases: usize) -> Scalar {
        if cases <= 1 << 8 {
            Scalar::U8
        } else if cases <= 1 << 16 {
            Scalar::U16
        } else {
            Scalar::U32
        }
    }
    /// The field types of `ty`'s record, or of its case `case`, substituted.
    pub(super) fn compact_fields(&mut self, ty: TyId, case: Option<u32>) -> Option<Vec<TyId>> {
        let (d, args) = match self.t.kind(self.bare(ty)) {
            TyKind::Adt(d, args) => (*d, args.clone()),
            TyKind::Tuple(xs) => return Some(xs.clone()),
            _ => return None,
        };
        let fields: Vec<TyId> = match (&self.p.decls[d.index()].body, case) {
            (DeclBody::Record(fs), None) => fs.iter().map(|f| f.ty).collect(),
            (DeclBody::Variant(cs), Some(c)) => {
                cs[c as usize].fields.iter().map(|f| f.ty).collect()
            }
            _ => return None,
        };
        Some(
            fields
                .into_iter()
                .map(|f| subst(&mut self.t, f, &args))
                .collect(),
        )
    }
    pub(super) fn variant_cases(&self, ty: TyId) -> Option<usize> {
        match self.t.kind(self.bare(ty)) {
            TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                DeclBody::Variant(cs) => Some(cs.len()),
                _ => None,
            },
            _ => None,
        }
    }
    /// The blob, offset and `W` of the locator `header` points at.
    pub(super) fn locator(
        &mut self,
        header: ValueId,
        n: NodeId,
    ) -> Option<(ValueId, ValueId, ValueId)> {
        if !self.live() {
            return None;
        }
        let blob = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let at = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let w = self.read_sequence_word(blob, 8, Ty::Scalar(Scalar::U64), n);
        Some((blob, at, w))
    }
    /// A new locator of `ty` for `{blob, at}`.
    pub(super) fn new_locator(
        &mut self,
        ty: TyId,
        blob: ValueId,
        at: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let Ok(Rep::Place(id)) = self.rep(ty) else {
            return None;
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        for (offset, v) in [(0, blob), (8, at)] {
            let o = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
            let t = self.b.values[v.index()];
            let a = self.address(
                AddressBase::Address(to),
                &[Step::Byte(o)],
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
        Some(to)
    }
    pub(super) fn add(&mut self, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::Add,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }
    pub(super) fn add_k(&mut self, left: ValueId, k: u64, n: NodeId) -> Option<ValueId> {
        if k == 0 {
            return Some(left);
        }
        let k = self.konst(Ty::Scalar(Scalar::U64), k, n)?;
        Some(self.add(left, k, n))
    }
    pub(super) fn mul(&mut self, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::Mul,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }
    /// The `W`-width word at `blob + at`, widened.
    pub(super) fn word_in(
        &mut self,
        blob: ValueId,
        at: ValueId,
        w: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        self.packed_count(blob, at, w, n)
    }
    /// The scalar of width `s` at `blob + at`, widened to `u32` (a tag).
    fn tag_at(&mut self, blob: ValueId, at: ValueId, s: Scalar, n: NodeId) -> ValueId {
        let a = self.address(
            AddressBase::Value(blob),
            &[Step::Byte(at)],
            PlaceTy::Value(Ty::Scalar(s)),
            n,
        );
        let v = self.read_bits(a, Ty::Scalar(s), n);
        if s == Scalar::U32 {
            return v;
        }
        let wide = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Convert {
                result: wide,
                flag: None,
                operand: v,
                mode: ConvertMode::Widen,
            },
            n,
        );
        wide
    }
    /// The tag of the compact variant `header` locates.
    pub(super) fn compact_tag(&mut self, header: ValueId, ty: TyId, n: NodeId) -> Option<ValueId> {
        let cases = self.variant_cases(ty)?;
        let (blob, at, _) = self.locator(header, n)?;
        Some(self.tag_at(blob, at, Self::tag_scalar(cases), n))
    }
    /// The byte after the encoding of a `ty` at `blob + at` (MEM-9..12): work fixed by
    /// the type, never by the contents.
    pub(super) fn skip(
        &mut self,
        blob: ValueId,
        at: ValueId,
        w: ValueId,
        ty: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let ty = self.bare(ty);
        match self.t.kind(ty).clone() {
            TyKind::Scalar(s) => return self.add_k(at, s.size(), n),
            TyKind::Unit => return Some(at),
            _ => {}
        }
        if let Some((role, args)) = self.role_of(ty) {
            return match role {
                Role::Text => {
                    let count = self.word_in(blob, at, w, n)?;
                    let body = self.add(at, w, n);
                    Some(self.add(body, count, n))
                }
                Role::FixedSpan => {
                    let count = self.word_in(blob, at, w, n)?;
                    let stride = self.packed_plain_size(args[0])?;
                    let stride = self.konst(Ty::Scalar(Scalar::U64), stride, n)?;
                    let bytes = self.mul(count, stride, n);
                    let body = self.add(at, w, n);
                    Some(self.add(body, bytes, n))
                }
                Role::SeqSpan | Role::IndexedSpan => {
                    let len_at = self.add(at, w, n);
                    let len = self.word_in(blob, len_at, w, n)?;
                    let body = self.add(len_at, w, n);
                    let mut end = self.add(body, len, n);
                    if role == Role::IndexedSpan {
                        let count = self.word_in(blob, at, w, n)?;
                        let table = self.mul(count, w, n);
                        end = self.add(end, table, n);
                    }
                    Some(end)
                }
                Role::Link => Some(self.add(at, w, n)),
                _ => None,
            };
        }
        if let Some(size) = self.packed_plain_size(ty) {
            return self.add_k(at, size, n);
        }
        if let Some(cases) = self.variant_cases(ty) {
            if self.link_option(ty).is_some() {
                return Some(self.add(at, w, n));
            }
            let tag_s = Self::tag_scalar(cases);
            let tag = self.tag_at(blob, at, tag_s, n);
            let start = self.add_k(at, tag_s.size(), n)?;
            let done = self.new_block(&[Ty::Scalar(Scalar::U64)]);
            for c in 0..cases as u32 {
                // The last case is reached by elimination.
                let last = c + 1 == cases as u32;
                let next = (!last).then(|| self.new_block(&[]));
                if let Some(next) = next {
                    let k = self.konst(Ty::Scalar(Scalar::U32), u64::from(c), n)?;
                    let here = self.value(Ty::Scalar(Scalar::Bool));
                    self.inst(
                        Inst::Compare {
                            result: here,
                            op: CompareOp::Eq,
                            left: tag,
                            right: k,
                        },
                        n,
                    );
                    let body = self.new_block(&[]);
                    let (yes, no) = (self.edge(body, &[]), self.edge(next, &[]));
                    self.end(
                        Terminator::Branch {
                            condition: here,
                            yes,
                            no,
                        },
                        Some(n),
                    );
                    self.start(body);
                }
                let mut off = start;
                for f in self.compact_fields(ty, Some(c))? {
                    off = self.skip(blob, off, w, f, n)?;
                }
                self.jump(done, &[off], n);
                if let Some(next) = next {
                    self.start(next);
                }
            }
            self.start(done);
            return Some(self.block_params(done)[0]);
        }
        let mut off = at;
        for f in self.compact_fields(ty, None)? {
            off = self.skip(blob, off, w, f, n)?;
        }
        Some(off)
    }
    /// `Option[Link[U]]`'s `U` (MEM-10: zero is `None`).
    fn link_option(&self, ty: TyId) -> Option<TyId> {
        let TyKind::Adt(d, args) = self.t.kind(self.bare(ty)) else {
            return None;
        };
        if !crate::decls::is_core_prelude(&self.p.decls[d.index()].path, &["Option"]) {
            return None;
        }
        match self.role_of(args[0]) {
            Some((Role::Link, inner)) => Some(inner[0]),
            _ => None,
        }
    }
    /// The place of field `field` (of case `case`) of the compact `ty` that `header`
    /// locates, and the field's type: a scalar or plain value decoded into a slot, a
    /// link followed to its target (MEM-10), anything else a locator in the same blob.
    pub(super) fn compact_field(
        &mut self,
        header: ValueId,
        ty: TyId,
        case: Option<u32>,
        field: u32,
        n: NodeId,
    ) -> Option<(AddressId, TyId)> {
        let fields = self.compact_fields(ty, case)?;
        let (blob, at, w) = self.locator(header, n)?;
        let mut off = at;
        if case.is_some() {
            let cases = self.variant_cases(ty)?;
            off = self.add_k(off, Self::tag_scalar(cases).size(), n)?;
        }
        for &f in &fields[..field as usize] {
            off = self.skip(blob, off, w, f, n)?;
        }
        let fty = fields[field as usize];
        let a = self.compact_value_at(blob, off, w, fty, n)?;
        Some((a, self.linked(fty)))
    }
    /// The type a field of type `ty` reads as: a link reads as a view of its target
    /// (MEM-10), anything else as itself.
    pub(super) fn linked(&mut self, ty: TyId) -> TyId {
        match self.role_of(ty) {
            Some((Role::Link, args)) => self.t.mk(TyKind::View(args[0])),
            _ => ty,
        }
    }
    /// The value of type `fty` encoded at `blob + off`, as a place.
    pub(super) fn compact_value_at(
        &mut self,
        blob: ValueId,
        off: ValueId,
        w: ValueId,
        fty: TyId,
        n: NodeId,
    ) -> Option<AddressId> {
        let bare = self.bare(fty);
        if let Some((Role::Link, args)) = self.role_of(bare) {
            let distance = self.word_in(blob, off, w, n)?;
            let target = self.value(Ty::Scalar(Scalar::U64));
            self.inst(
                Inst::Binary {
                    result: target,
                    op: BinaryOp::Sub,
                    left: off,
                    right: distance,
                    fail: None,
                },
                n,
            );
            return self.new_locator(args[0], blob, target, n);
        }
        if self.link_option(bare).is_some() {
            self.gap(n, "an optional link field is not built");
            return None;
        }
        match self.rep(bare) {
            Ok(Rep::Scalar(t)) => {
                let a = self.address(
                    AddressBase::Value(blob),
                    &[Step::Byte(off)],
                    PlaceTy::Value(t),
                    n,
                );
                let v = self.read_bits(a, t, n);
                let to = self.new_slot(PlaceTy::Value(t), n);
                self.inst(
                    Inst::Store {
                        address: to,
                        value: v,
                    },
                    n,
                );
                Some(to)
            }
            Ok(Rep::None) => None,
            Ok(Rep::Place(id)) => {
                if self.is_locator(bare) {
                    return self.new_locator(bare, blob, off, n);
                }
                if self.plain_fields(bare).is_some()
                    || self.private_variant_cases(bare).is_some()
                    || matches!(self.t.kind(bare), TyKind::Array(..))
                {
                    let to = self.new_slot(PlaceTy::Declared(id), n);
                    self.decode_plain(blob, off, w, bare, to, n)?;
                    return Some(to);
                }
                self.gap(n, "this compact field is not built");
                None
            }
            Err(what) => {
                self.gap(n, what);
                None
            }
        }
    }
    /// A value of `ty` is held as a locator into a blob: a compact value, `Text` or a
    /// stable sequence.
    pub(super) fn is_locator(&self, ty: TyId) -> bool {
        self.compact_adt(ty).is_some()
            || matches!(
                self.role_of(ty),
                Some((
                    Role::Text
                        | Role::FixedSpan
                        | Role::SeqSpan
                        | Role::IndexedSpan
                        | Role::SeqIndex,
                    _
                ))
            )
    }
    /// A field of a temporary, such as a call result (OWN-8): the base is evaluated
    /// once and read in place. Through a view, or in a stable value, a field that is
    /// not plain reads as a view.
    pub(super) fn temporary_field(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let (base, value) = self.expr(self.kids(n)[0], None);
        if base == ERROR {
            return (ERROR, None);
        }
        let view = matches!(self.t.kind(base), TyKind::View(_));
        let inner = self.bare(base);
        let name = self.token_text(n);
        let (found, stable) = match self.t.kind(inner).clone() {
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                let stable = decl.repr == Repr::Stable;
                let found = match &decl.body {
                    DeclBody::Record(fs) => fs
                        .iter()
                        .position(|f| f.name == name)
                        .map(|i| (i, subst(&mut self.t, fs[i].ty, &args))),
                    _ => None,
                };
                (found, stable)
            }
            _ => (None, false),
        };
        let Some((i, ft)) = found else {
            return self.gap_ty(n, "this field access is not built");
        };
        if !view && !self.copies(inner) {
            return self.gap_ty(n, "a field of a temporary owner is not built");
        }
        let compact = self.compact_adt(inner).is_some();
        let held = self.rep(base).ok().and_then(Rep::pointee);
        if !compact && held.is_none() {
            return self.gap_ty(n, "a field of this temporary is not built");
        }
        let ft = self.linked(ft);
        let ty = if (view || stable)
            && !self.copies(ft)
            && !matches!(self.t.kind(ft), TyKind::View(_))
        {
            self.t.mk(TyKind::View(ft))
        } else {
            ft
        };
        let v = (|| {
            let v = value?;
            if !self.live() {
                return None;
            }
            let address = if compact {
                self.compact_field(v, inner, None, i as u32, n)?.0
            } else {
                let id = held?;
                let place = self.place_ty(ft, n)?;
                let at = self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n);
                self.address(AddressBase::Address(at), &[Step::Field(i as u32)], place, n)
            };
            match self.rep(ft).ok()? {
                r @ Rep::Scalar(_) => Some(self.read_bits(address, r.ty()?, n)),
                Rep::Place(_) if ty != ft => Some(self.pointer(address, n)),
                Rep::Place(id) => {
                    let p = self.pointer(address, n);
                    Some(self.snapshot(p, id, n))
                }
                Rep::None => None,
            }
        })();
        (ty, v)
    }
}
