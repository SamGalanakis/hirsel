//! `Inst::Compare` over ledger values: the interpreter's structural order
//! (`hirsel_interp::structural::compare`, reimplemented here because this crate
//! never depends on the interpreter) read from layouts. Pairs are visited depth
//! first in field order and the first unequal pair decides.

use super::*;
use std::cmp::Ordering;

/// Where one side of a pair lives: inside a value image or in an allocation.
#[derive(Clone, Copy)]
pub(super) enum At<'e> {
    Image(&'e Extent, u64),
    Heap(Ptr),
}

/// One step of a comparison: a pair to compare, or two sequences' lengths,
/// decided once their common elements are equal.
enum Item<'e> {
    Pair(At<'e>, At<'e>, LayoutId),
    Lengths(u64, u64),
}

impl At<'_> {
    pub(super) fn at(self, offset: u64) -> Self {
        match self {
            At::Image(e, o) => At::Image(e, o + offset),
            At::Heap(p) => At::Heap(p.at(offset)),
        }
    }
}

impl<S: Stage> Machine<'_, S> {
    /// The structural order of `a` and `b` of `layout`; `None` is unordered.
    pub(super) fn structural_order(
        &self,
        a: &Val,
        b: &Val,
        layout: LayoutId,
    ) -> Checked<Option<Ordering>> {
        let mut work: Vec<Item<'_>> = Vec::new();
        match (a, b, &self.record(layout)?.kind) {
            (
                Val::Bits {
                    scalar: ScalarKind::U64,
                    bits: a,
                },
                Val::Bits {
                    scalar: ScalarKind::U64,
                    bits: b,
                },
                LayoutKind::Path,
            ) => return Ok(Some(self.path_order(*a as u64, *b as u64)?)),
            (Val::Unit, Val::Unit, LayoutKind::Unit) => return Ok(Some(Ordering::Equal)),
            // SEM-5: an `ordered of` token orders by its unsigned word.
            (Val::Word(a), Val::Word(b), LayoutKind::Handle) => return Ok(Some(a.cmp(b))),
            (
                Val::Ptr(pa),
                Val::Ptr(pb),
                LayoutKind::Block { payload } | LayoutKind::Box { payload },
            ) => {
                self.block(At::Heap(*pa), At::Heap(*pb), *payload, &mut work)?;
            }
            (Val::Image(x), Val::Image(y), _) => {
                work.push(Item::Pair(At::Image(x, 0), At::Image(y, 0), layout));
            }
            (a, b, kind) => {
                return fail(
                    V::TypeConfusion,
                    format!("structural comparison of {a:?} and {b:?} as {kind:?}"),
                );
            }
        }
        self.order_work(work)
    }
    fn path_order(&self, a: u64, b: u64) -> Checked<Ordering> {
        if a == b {
            return Ok(Ordering::Equal);
        }
        let a = self
            .ops
            .path_text(a)
            .ok_or_else(|| err(V::Malformed, "unknown Path ID"))?;
        let b = self
            .ops
            .path_text(b)
            .ok_or_else(|| err(V::Malformed, "unknown Path ID"))?;
        Ok(a.as_bytes().cmp(b.as_bytes()))
    }
    fn order_work(&self, mut work: Vec<Item<'_>>) -> Checked<Option<Ordering>> {
        while let Some(item) = work.pop() {
            let (x, y, layout) = match item {
                Item::Pair(x, y, layout) => (x, y, layout),
                Item::Lengths(l, r) => match l.cmp(&r) {
                    Ordering::Equal => continue,
                    other => return Ok(Some(other)),
                },
            };
            let record = self.record(layout)?;
            let mut children = |fields: &[hirsel_lir::layout_types::FieldLayout]| {
                let mut sorted: Vec<_> = fields.iter().collect();
                sorted.sort_by_key(|f| f.ordinal);
                for f in sorted.into_iter().rev() {
                    work.push(Item::Pair(x.at(f.offset), y.at(f.offset), f.layout));
                }
            };
            match &record.kind {
                LayoutKind::Path => {
                    let order =
                        self.path_order(self.bits_at(x, 8)? as u64, self.bits_at(y, 8)? as u64)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                LayoutKind::Unit => {}
                LayoutKind::Handle => {
                    let order = self.bits_at(x, 8)?.cmp(&self.bits_at(y, 8)?);
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                LayoutKind::Scalar(kind) => {
                    let size = self.size(layout)?;
                    let (l, r) = (self.bits_at(x, size)?, self.bits_at(y, size)?);
                    let (l, r) = (scalar::canonical(*kind, l), scalar::canonical(*kind, r));
                    let order = scalar::order(*kind, l, r).map_err(|m| err(V::TypeConfusion, m))?;
                    match order {
                        Some(Ordering::Equal) => {}
                        other => return Ok(other),
                    }
                }
                LayoutKind::Aggregate => children(&record.fields),
                LayoutKind::Variant => {
                    let tag = record
                        .tag
                        .as_ref()
                        .ok_or_else(|| err(V::Malformed, "variant layout without tag"))?;
                    let (l, r) = (
                        self.tag_at(x.at(tag.offset), tag.form)?,
                        self.tag_at(y.at(tag.offset), tag.form)?,
                    );
                    if l != r {
                        return Ok(Some(l.cmp(&r)));
                    }
                    let variant = tag
                        .variants
                        .iter()
                        .find(|v| v.ordinal == l)
                        .ok_or_else(|| err(V::Malformed, format!("unknown variant tag {l}")))?;
                    children(&variant.fields);
                }
                // Simd lanes compare as inline elements, in lane order.
                LayoutKind::Inline { .. } | LayoutKind::Simd { .. } => {
                    let (element, count) = match record.kind {
                        LayoutKind::Inline { element, count } => (element, count),
                        LayoutKind::Simd { element, lanes } => (element, u64::from(lanes)),
                        _ => unreachable!(),
                    };
                    let (element, count) = (&element, &count);
                    let stride = self.size(*element)?;
                    for i in (0..*count).rev() {
                        work.push(Item::Pair(x.at(i * stride), y.at(i * stride), *element));
                    }
                }
                LayoutKind::Block { payload } | LayoutKind::Box { payload } => {
                    let (l, r) = (self.ptr_at(x)?, self.ptr_at(y)?);
                    self.block(At::Heap(l), At::Heap(r), *payload, &mut work)?;
                }
                // String and Bytes: bytewise, a proper prefix first (as emission's
                // comparator reads them).
                LayoutKind::Slice { element } => {
                    if self.size(*element)? != 1 {
                        return fail(V::Unsupported, "structural comparison of a wide slice");
                    }
                    let (ex, ox) = self.extent_at(x)?;
                    let (ey, oy) = self.extent_at(y)?;
                    let l = self.slice_content(ex, ox, layout)?;
                    let r = self.slice_content(ey, oy, layout)?;
                    match l.cmp(&r) {
                        Ordering::Equal => {}
                        other => return Ok(Some(other)),
                    }
                }
                LayoutKind::Opaque { owner }
                    if owner.0.package.name_is("core")
                        && matches!(owner.0.name.as_str(), "Map" | "Set") =>
                {
                    let entries = &record.fields[0];
                    let (x, y) = (
                        self.ptr_at(x.at(entries.offset))?,
                        self.ptr_at(y.at(entries.offset))?,
                    );
                    let payload = match self.record(entries.layout)?.kind {
                        LayoutKind::Block { payload } | LayoutKind::Box { payload } => payload,
                        _ => return fail(V::Malformed, "container entries"),
                    };
                    let mut element = match self.record(payload)?.kind {
                        LayoutKind::Sequence { element } => element,
                        _ => return fail(V::Malformed, "container sequence"),
                    };
                    let (mut xc, mut yc) = (
                        self.bits_at(At::Heap(x), 8)? as u64,
                        self.bits_at(At::Heap(y), 8)? as u64,
                    );
                    let (start, stride) = self.sequence_shape(element)?;
                    let mut xs: Vec<_> = (0..xc)
                        .map(|i| At::Heap(x.at(start + i * stride)))
                        .collect();
                    let mut ys: Vec<_> = (0..yc)
                        .map(|i| At::Heap(y.at(start + i * stride)))
                        .collect();
                    if matches!(owner.0.name.as_str(), "Map" | "Set") {
                        let tag = self
                            .record(element)?
                            .tag
                            .as_ref()
                            .ok_or_else(|| err(V::Malformed, "map entry tag"))?;
                        let pair = tag
                            .variants
                            .iter()
                            .find(|v| v.ordinal == 1)
                            .and_then(|v| v.fields.first())
                            .ok_or_else(|| err(V::Malformed, "map entry pair"))?;
                        let mut left = Vec::new();
                        let mut right = Vec::new();
                        for item in xs {
                            if self.tag_at(item.at(tag.offset), tag.form)? == 1 {
                                left.push(item.at(pair.offset));
                            }
                        }
                        for item in ys {
                            if self.tag_at(item.at(tag.offset), tag.form)? == 1 {
                                right.push(item.at(pair.offset));
                            }
                        }
                        xs = left;
                        ys = right;
                        xc = xs.len() as u64;
                        yc = ys.len() as u64;
                        element = pair.layout;
                        if owner.0.name == "Set" {
                            let key = &self.record(element)?.fields[0];
                            xs = xs.into_iter().map(|at| at.at(key.offset)).collect();
                            ys = ys.into_iter().map(|at| at.at(key.offset)).collect();
                            element = key.layout;
                        }
                    }
                    if owner.0.name == "Set" {
                        // SEM-5, #93: structural sorted element order.
                        let mut invariant = None;
                        let mut compare = |a: &At<'_>, b: &At<'_>| match self
                            .order_work(vec![Item::Pair(*a, *b, element)])
                        {
                            Ok(Some(order)) => order,
                            Ok(None) => {
                                invariant = Some(err(V::TypeConfusion, "unordered Set key"));
                                Ordering::Equal
                            }
                            Err(error) => {
                                invariant = Some(error);
                                Ordering::Equal
                            }
                        };
                        xs.sort_by(&mut compare);
                        ys.sort_by(&mut compare);
                        if let Some(error) = invariant {
                            return Err(error);
                        }
                    }
                    for (x, y) in xs.into_iter().zip(ys) {
                        match self.order_work(vec![Item::Pair(x, y, element)])? {
                            Some(Ordering::Equal) => {}
                            other => return Ok(other),
                        }
                    }
                    if xc != yc {
                        return Ok(Some(xc.cmp(&yc)));
                    }
                }
                LayoutKind::Sequence { .. } => {
                    self.block(x, y, layout, &mut work)?;
                }
                LayoutKind::Packed
                | LayoutKind::Sequential { .. }
                | LayoutKind::Opaque { .. }
                | LayoutKind::Flat
                | LayoutKind::SoaColumns { .. } => {
                    return fail(
                        V::Unsupported,
                        format!("structural comparison of {:?}", record.kind),
                    );
                }
                LayoutKind::Environment { .. }
                | LayoutKind::HostCapability { .. }
                | LayoutKind::SealedRegion { .. }
                | LayoutKind::PayloadReceipt { .. }
                | LayoutKind::Arena
                | LayoutKind::Callable { .. }
                | LayoutKind::Iterator { .. }
                | LayoutKind::Resource
                | LayoutKind::Shared { .. }
                | LayoutKind::Frozen { .. }
                | LayoutKind::Header => {
                    return fail(
                        V::TypeConfusion,
                        format!("structural comparison of {:?}", record.kind),
                    );
                }
            }
        }
        Ok(Some(Ordering::Equal))
    }

    /// `Inst::Hash`: a hash of `value` at `layout` that equal values (by
    /// `Compare Eq`) share. Its bits are unobservable and need not agree
    /// with compiled code (SEM-5: a hash decides placement only), so this
    /// is a plain FNV-1a fold of the scalar leaves in layout order, with
    /// `-0.0` folded as `+0.0`.
    pub(super) fn structural_hash(&self, value: &Val, layout: LayoutId) -> Checked<u64> {
        let at = match value {
            Val::Unit => return Ok(0xcbf2_9ce4_8422_2325),
            Val::Bits { scalar, bits } => {
                return self.hash_bits(*scalar, *bits);
            }
            Val::Ptr(p) => {
                let (LayoutKind::Block { payload } | LayoutKind::Box { payload }) =
                    self.record(layout)?.kind
                else {
                    return fail(V::TypeConfusion, "hash pointer layout");
                };
                return self.hash_work(vec![(At::Heap(*p), payload)]);
            }
            Val::Image(image) => At::Image(image, 0),
            value => return fail(V::TypeConfusion, format!("hash {value:?}")),
        };
        self.hash_work(vec![(at, layout)])
    }
    fn hash_bits(&self, scalar: ScalarKind, bits: u128) -> Checked<u64> {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for b in hash_leaf(scalar, bits).to_le_bytes() {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        Ok(hash)
    }
    fn hash_work(&self, mut work: Vec<(At<'_>, LayoutId)>) -> Checked<u64> {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut fold = |bytes: &[u8]| {
            for b in bytes {
                hash = (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        // Depth first in layout order: children are pushed in reverse.
        while let Some((at, layout)) = work.pop() {
            let record = self.record(layout)?;
            let children = |fields: &[hirsel_lir::layout_types::FieldLayout]| {
                let mut sorted: Vec<_> = fields
                    .iter()
                    .map(|f| (f.ordinal, f.offset, f.layout))
                    .collect();
                sorted.sort_by_key(|f| std::cmp::Reverse(f.0));
                sorted
            };
            match record.kind {
                LayoutKind::Path => {
                    fold(&hash_leaf(ScalarKind::U64, self.bits_at(at, 8)?).to_le_bytes());
                }
                LayoutKind::Unit => {}
                LayoutKind::Scalar(kind) => {
                    let size = self.size(layout)?;
                    fold(&hash_leaf(kind, self.bits_at(at, size)?).to_le_bytes());
                }
                LayoutKind::Aggregate => {
                    for (_, offset, layout) in children(&record.fields) {
                        work.push((at.at(offset), layout));
                    }
                }
                LayoutKind::Inline { element, count } => {
                    let stride = self.size(element)?;
                    for i in (0..count).rev() {
                        work.push((at.at(i * stride), element));
                    }
                }
                LayoutKind::Simd { element, lanes } => {
                    let stride = self.size(element)?;
                    for i in (0..u64::from(lanes)).rev() {
                        work.push((at.at(i * stride), element));
                    }
                }
                LayoutKind::Variant => {
                    let tag = record
                        .tag
                        .as_ref()
                        .ok_or_else(|| err(V::Malformed, "a variant layout without a tag"))?;
                    let active = self.tag_at(at.at(tag.offset), tag.form)?;
                    fold(&active.to_le_bytes());
                    let variant = tag
                        .variants
                        .iter()
                        .find(|v| v.ordinal == active)
                        .ok_or_else(|| err(V::Malformed, "unknown variant"))?;
                    for (_, offset, layout) in children(&variant.fields) {
                        work.push((at.at(offset), layout));
                    }
                }
                LayoutKind::Block { payload } | LayoutKind::Box { payload } => {
                    let target = self.ptr_at(at)?;
                    work.push((At::Heap(target), payload));
                }
                LayoutKind::Sequence { element } => {
                    let count = self.bits_at(at, 8)? as u64;
                    fold(&count.to_le_bytes());
                    let start = hirsel_lir::layout::sequence_elements_offset(self.record(element)?)
                        .map_err(|_| err(V::Malformed, "a sequence element offset"))?;
                    let stride = self.size(element)?;
                    for i in (0..count).rev() {
                        work.push((at.at(start + i * stride), element));
                    }
                }
                LayoutKind::Slice { .. } => {
                    let (extent, offset) = self.extent_at(at)?;
                    let bytes = self.slice_content(extent, offset, layout)?;
                    fold(&(bytes.len() as u64).to_le_bytes());
                    fold(&bytes);
                }
                LayoutKind::Opaque { ref owner }
                    if owner.0.package.name_is("core")
                        && matches!(owner.0.name.as_str(), "Map" | "Set") =>
                {
                    let field = &record.fields[0];
                    let entries = self.ptr_at(at.at(field.offset))?;
                    let payload = match self.record(field.layout)?.kind {
                        LayoutKind::Block { payload } | LayoutKind::Box { payload } => payload,
                        _ => return fail(V::Malformed, "container entries"),
                    };
                    let element = match self.record(payload)?.kind {
                        LayoutKind::Sequence { element } => element,
                        _ => return fail(V::Malformed, "container sequence"),
                    };
                    let count = self.bits_at(At::Heap(entries), 8)? as u64;
                    let live = if matches!(owner.0.name.as_str(), "Map" | "Set") {
                        self.bits_at(at.at(record.fields[3].offset), 8)? as u64
                    } else {
                        count
                    };
                    fold(&live.to_le_bytes());
                    let (start, stride) = self.sequence_shape(element)?;
                    if owner.0.name == "Set" {
                        let tag = self.record(element)?.tag.as_ref().unwrap();
                        let pair = &tag.variants.iter().find(|v| v.ordinal == 1).unwrap().fields[0];
                        let key = &self.record(pair.layout)?.fields[0];
                        let mut combined = 0u64;
                        for i in 0..count {
                            let entry = At::Heap(entries.at(start + i * stride));
                            if self.tag_at(entry.at(tag.offset), tag.form)? != 1 {
                                continue;
                            }
                            let child = self.hash_work(vec![(
                                entry.at(pair.offset + key.offset),
                                key.layout,
                            )])?;
                            combined = combined.wrapping_add(child);
                        }
                        fold(&combined.to_le_bytes());
                    } else {
                        let tag = self
                            .record(element)?
                            .tag
                            .as_ref()
                            .ok_or_else(|| err(V::Malformed, "map entry tag"))?;
                        let pair = tag
                            .variants
                            .iter()
                            .find(|v| v.ordinal == 1)
                            .and_then(|v| v.fields.first())
                            .ok_or_else(|| err(V::Malformed, "map entry pair"))?;
                        for i in (0..count).rev() {
                            let entry = At::Heap(entries.at(start + i * stride));
                            if self.tag_at(entry.at(tag.offset), tag.form)? == 1 {
                                work.push((entry.at(pair.offset), pair.layout));
                            }
                        }
                    }
                }
                LayoutKind::Packed
                | LayoutKind::Sequential { .. }
                | LayoutKind::Opaque { .. }
                | LayoutKind::Flat
                | LayoutKind::SoaColumns { .. } => {
                    return fail(V::Unsupported, format!("a hash of {:?}", record.kind));
                }
                LayoutKind::Environment { .. }
                | LayoutKind::Handle
                | LayoutKind::HostCapability { .. }
                | LayoutKind::SealedRegion { .. }
                | LayoutKind::PayloadReceipt { .. }
                | LayoutKind::Arena
                | LayoutKind::Callable { .. }
                | LayoutKind::Iterator { .. }
                | LayoutKind::Resource
                | LayoutKind::Shared { .. }
                | LayoutKind::Frozen { .. }
                | LayoutKind::Header => {
                    return fail(V::TypeConfusion, format!("a hash of {:?}", record.kind));
                }
            }
        }
        Ok(hash)
    }

    /// The pairs of two blocks of `payload`, as emission's comparator reads
    /// them: a Box's payload; a List's or Array's common elements in order,
    /// then their lengths, a proper prefix first.
    fn block<'e>(
        &self,
        x: At<'e>,
        y: At<'e>,
        payload: LayoutId,
        work: &mut Vec<Item<'e>>,
    ) -> Checked<()> {
        let LayoutKind::Sequence { element } = self.record(payload)?.kind else {
            work.push(Item::Pair(x, y, payload));
            return Ok(());
        };
        let (l, r) = (self.bits_at(x, 8)? as u64, self.bits_at(y, 8)? as u64);
        let start = hirsel_lir::layout::sequence_elements_offset(self.record(element)?)
            .map_err(|_| err(V::Malformed, "a sequence element offset"))?;
        let stride = self.size(element)?;
        work.push(Item::Lengths(l, r));
        for i in (0..l.min(r)).rev() {
            let at = start + i * stride;
            work.push(Item::Pair(x.at(at), y.at(at), element));
        }
        Ok(())
    }

    pub(super) fn extent_at<'e>(&'e self, at: At<'e>) -> Checked<(&'e Extent, u64)> {
        Ok(match at {
            At::Image(e, offset) => (e, offset),
            At::Heap(p) => (&self.ledger.check(p)?.extent, p.offset),
        })
    }

    pub(super) fn bits_at(&self, at: At<'_>, size: u64) -> Checked<u128> {
        let (e, offset) = self.extent_at(at)?;
        e.check_range(offset, size)?;
        if !e.all_init(offset, size) {
            return fail(
                V::UninitializedRead,
                format!("compared {size} bytes at offset {offset} are not initialized"),
            );
        }
        let mut bytes = [0u8; 16];
        bytes[..size as usize].copy_from_slice(e.read_bytes(offset, size));
        Ok(u128::from_le_bytes(bytes))
    }

    /// The active ordinal of a variant whose tag, read as `form`, is at `at`.
    pub(super) fn tag_at(&self, at: At<'_>, form: TagForm) -> Checked<u32> {
        let (e, offset) = self.extent_at(at)?;
        e.active(offset, form, self.word)
    }

    pub(super) fn ptr_at(&self, at: At<'_>) -> Checked<Ptr> {
        let (e, offset) = self.extent_at(at)?;
        e.check_range(offset, self.word)?;
        match e.cells.get(&offset) {
            Some(Cell::Data { ptr, .. }) => {
                self.ledger.check(*ptr)?;
                Ok(*ptr)
            }
            _ => fail(
                V::ForgedPointer,
                "a compared block reference is not a handle",
            ),
        }
    }
}

/// A scalar leaf as hashed: equal values share bits, so `-0.0` is `+0.0`.
fn hash_leaf(kind: ScalarKind, bits: u128) -> u128 {
    match kind {
        ScalarKind::F32 if bits as u32 == 0x8000_0000 => 0,
        ScalarKind::F64 if bits as u64 == 0x8000_0000_0000_0000 => 0,
        _ => bits,
    }
}
