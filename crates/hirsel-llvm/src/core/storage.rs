//! Recipe-driven storage and runtime memory operations.
use super::*;
use hirsel_core_layout::{FieldTy, Layout, Repr, TypeRef, field_size, held_size};

/// A C argument the x86-64 ABI zero-extends.
pub(super) fn zeroext(ty: Type, value: impl Into<Value>) -> CArg {
    (Parameter::extended(ty, Extension::Zero), value.into())
}

/// Where an allocation is charged, resolved to operands.
pub(super) enum Payer {
    Current,
    /// Like the block at this pointer (null charges the current limit).
    Region(Value),
}

#[derive(Clone, Copy)]
pub(super) enum PointerOrigin {
    Address(AddressId),
    Read(AddressId),
    Static(StaticId),
    Parameter(PlaceTy),
}

pub(super) fn pointer_origins(body: &Body, sig: &Signature) -> Box<[Option<PointerOrigin>]> {
    let mut origins = vec![None; body.values.len()];
    if !body.edges.iter().any(|edge| edge.target.index() == 0) {
        let parameters =
            sig.dest
                .map(PlaceTy::Declared)
                .into_iter()
                .map(Some)
                .chain(sig.params.iter().map(|param| match *param {
                    Param::Address { place, .. } => Some(place),
                    Param::Value(_) => None,
                }));
        let entry = body.blocks[0].params.get(&body.pools.values).unwrap();
        for (&value, place) in entry.iter().zip(parameters) {
            origins[value.index()] = place.map(PointerOrigin::Parameter);
        }
    }
    for inst in &body.insts {
        match *inst {
            Inst::Pointer { result, address } => {
                origins[result.index()] = Some(PointerOrigin::Address(address));
            }
            Inst::ReadStatic { result, data } => {
                origins[result.index()] = Some(PointerOrigin::Static(data));
            }
            Inst::Read {
                result, address, ..
            } if body.values[result.index()] == Ty::Ptr => {
                origins[result.index()] = Some(PointerOrigin::Read(address));
            }
            _ => {}
        }
    }
    origins.into_boxed_slice()
}

#[derive(Clone)]
pub(super) enum At {
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(Box<FieldTy>, u64),
    /// The `seq` field of a carrier row, with its element.
    Seq(Box<FieldTy>),
    Other,
}
impl FunctionEmitter<'_> {
    pub(super) fn fixed(&self, t: TypeId) -> &hirsel_core_layout::Fixed {
        let Layout::Fixed(f) = &self.recipes[t.index()].layout else {
            panic!("compact projection requires its extent recipe")
        };
        f
    }
    /// A resource or region value is held as a pointer to its block (MEM-5, MEM-24).
    fn by_block(&self, t: TypeId) -> bool {
        matches!(
            self.module.types[t.index()].repr,
            Repr::Resource | Repr::Region
        )
    }
    fn kind_of(&self, t: TypeId) -> At {
        if self.by_block(t) {
            At::Pointer(t)
        } else {
            At::Declared(t)
        }
    }
    fn field_kind(&self, t: &FieldTy) -> At {
        match t {
            FieldTy::Inline(t) => self.kind_of(TypeId(t.0)),
            FieldTy::Box(t) => At::Pointer(TypeId(t.0)),
            FieldTy::Rc(t) | FieldTy::Arc(t) | FieldTy::Shared(t) => At::Counted(TypeId(t.0)),
            FieldTy::Array(e, n) => At::Array(e.clone(), *n),
            FieldTy::Seq(e) => At::Seq(e.clone()),
            _ => At::Other,
        }
    }
    /// The pieces, as (offset, width) of naturally aligned integers, that a
    /// whole-value copy of fixed own type `t` moves instead of all its bytes.
    /// Padding is private (MEM-13): no read observes it. Copying a gap after a
    /// narrow tag reads a word that the tag's narrow store only partly wrote,
    /// and that reload misses store forwarding. Like rustc, which copies Scalar
    /// and ScalarPair values as typed scalars and everything else with memcpy,
    /// only a value whose occupied bytes are at most two such pieces and leave
    /// a hole is split; a wider split costs more moves than the whole copy and
    /// hides the copy from memcpy forwarding.
    pub(super) fn copy_pieces(&self, t: TypeId) -> Option<Vec<(u64, u64)>> {
        let mut ranges = Vec::new();
        self.occupied(t, 0, &mut ranges)?;
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        if merged == [(0, self.fixed(t).size)] {
            return None;
        }
        let mut pieces = Vec::new();
        for (start, end) in merged {
            let mut at = start;
            while at < end {
                let width = [8, 4, 2, 1]
                    .into_iter()
                    .find(|w| at % w == 0 && at + w <= end)
                    .expect("a one-byte piece fits");
                pieces.push((at, width));
                at += width;
            }
        }
        (pieces.len() <= 2).then_some(pieces)
    }
    /// Push the byte ranges that the tag or some case's data of fixed own type
    /// `t` at offset `at` occupies. None when the value is too fragmented to
    /// be worth splitting.
    fn occupied(&self, t: TypeId, at: u64, out: &mut Vec<(u64, u64)>) -> Option<()> {
        let Layout::Fixed(f) = &self.recipes[t.index()].layout else {
            return None;
        };
        if self.by_block(t) {
            out.push((at, at + 8));
            return Some(());
        }
        if let Some(tag) = f.tag {
            out.push((at + tag.offset, at + tag.offset + tag.width));
        }
        let shape = &self.module.types[t.index()];
        for (case, offsets) in f.offsets.iter().enumerate() {
            let case = f.tag.map(|_| case as u32);
            for (field, &offset) in shape.fields(case)?.iter().zip(offsets) {
                self.occupied_field(shape.repr, &field.ty, at + offset, out)?;
            }
        }
        (out.len() <= 64).then_some(())
    }
    fn occupied_field(
        &self,
        holder: Repr,
        ty: &FieldTy,
        at: u64,
        out: &mut Vec<(u64, u64)>,
    ) -> Option<()> {
        match ty {
            FieldTy::Inline(t) if matches!(self.recipes[t.index()].layout, Layout::Fixed(_)) => {
                self.occupied(TypeId(t.0), at, out)
            }
            FieldTy::Array(element, n) if *n <= 4 => {
                let stride = held_size(&self.module.types, self.recipes, holder, element).0;
                (0..*n).try_for_each(|i| self.occupied_field(holder, element, at + i * stride, out))
            }
            _ => {
                let size = held_size(&self.module.types, self.recipes, holder, ty).0;
                out.push((at, at + size));
                Some(())
            }
        }
    }
    pub(super) fn place_layout(&self, t: PlaceTy) -> (u64, u64) {
        match t {
            PlaceTy::Value(t) => (size(t), size(t)),
            PlaceTy::Declared(t) if self.by_block(t) => (8, 8),
            PlaceTy::Declared(t) => {
                let f = self.fixed(t);
                (f.size, f.align)
            }
        }
    }
    /// FORM #113: the exclusive end of a typed len or capacity read's value,
    /// `seq_len_bound` of the element's logical size (MEM-24), when there is one.
    pub(super) fn seq_word_range(&self, address: AddressId) -> Option<u64> {
        let info = &self.body.addresses[address.index()];
        let steps = info.steps.get(&self.body.pools.steps).unwrap();
        let (Step::Seq(SeqWord::Len | SeqWord::Cap), before) = steps.split_last()? else {
            return None;
        };
        let At::Seq(element) = self.walk(address, before.len()) else {
            unreachable!("a seq word is addressed through its carrier's seq field")
        };
        let size = field_size(&self.module.types, self.recipes, &element).2;
        Some(hirsel_core_layout::charge::seq_len_bound(size)? + 1)
    }
    /// The TBAA access type of a scalar read or store at `address`: a `seq`
    /// carrier word, or a scalar leaf that no carrier word can occupy. Any
    /// other place keeps the program storage type, which aliases both.
    pub(super) fn storage_memory(&self, address: AddressId) -> Option<usize> {
        let info = &self.body.addresses[address.index()];
        let steps = info.steps.get(&self.body.pools.steps).unwrap();
        let (last, before) = steps.split_last()?;
        let scalar = |t: &FieldTy| {
            matches!(t, FieldTy::Scalar(_) | FieldTy::Box(_) | FieldTy::Rc(_))
                || matches!(t, FieldTy::Arc(_) | FieldTy::Shared(_))
        };
        let data = match *last {
            Step::Seq(_) => return Some(alias::SEQ_WORD_ACCESS),
            Step::Byte(at) => {
                let PlaceTy::Value(ty) = info.ty else {
                    return None;
                };
                self.scalar_buffer(address)
                    || (before.is_empty()
                        && self.constants[at.index()]
                            .and_then(|offset| offset.checked_add(size(ty)))
                            .zip(self.inline_scalar_extent(info.base))
                            .is_some_and(|(end, extent)| end <= extent))
            }
            _ if steps.iter().any(|s| matches!(s, Step::Byte(_))) => false,
            Step::Index(_) => matches!(info.ty, PlaceTy::Value(_)),
            Step::Element(_) => {
                matches!(self.walk(address, before.len()), At::Array(e, _) if scalar(&e))
            }
            Step::Field(field) | Step::CaseField { field, .. } => {
                let case = match *last {
                    Step::CaseField { case, .. } => Some(case),
                    _ => None,
                };
                match self.walk(address, before.len()) {
                    At::Declared(t) => self.module.types[t.index()]
                        .fields(case)
                        .is_some_and(|f| scalar(&f[field as usize].ty)),
                    _ => false,
                }
            }
            Step::Deref => false,
        };
        data.then_some(alias::DATA_ACCESS)
    }
    /// A typed data word of a scalar sequence points into an element buffer.
    /// Its bytes hold no carriers. Raw projections preserve that fact for
    /// accesses within the buffer, which the CoreIR producer bounds.
    pub(super) fn scalar_buffer(&self, mut address: AddressId) -> bool {
        loop {
            let info = &self.body.addresses[address.index()];
            let steps = info.steps.get(&self.body.pools.steps).unwrap();
            if !steps
                .iter()
                .all(|s| matches!(s, Step::Byte(_) | Step::Index(_)))
            {
                return false;
            }
            match info.base {
                AddressBase::Slot(_) => return false,
                AddressBase::Address(parent) => address = parent,
                AddressBase::Value(value) => {
                    match self.pointer_origins.get(value.index()).copied().flatten() {
                        Some(PointerOrigin::Address(parent)) => address = parent,
                        Some(PointerOrigin::Read(source)) => {
                            let info = &self.body.addresses[source.index()];
                            let steps = info.steps.get(&self.body.pools.steps).unwrap();
                            let Some((Step::Seq(SeqWord::Data), before)) = steps.split_last()
                            else {
                                return false;
                            };
                            return matches!(
                                self.walk(source, before.len()),
                                At::Seq(element) if matches!(*element, FieldTy::Scalar(_))
                            );
                        }
                        Some(PointerOrigin::Parameter(_) | PointerOrigin::Static(_)) | None => {
                            return false;
                        }
                    }
                }
            }
        }
    }

    /// A constant byte access inside the physical scalar array cannot reach
    /// other fields in the same allocation. Leave dynamic Inline offsets
    /// unclassified when this projection has no extent proof.
    pub(super) fn inline_scalar_extent(&self, base: AddressBase) -> Option<u64> {
        let declared = match base {
            AddressBase::Slot(slot) => self.body.slots[slot.index()],
            AddressBase::Value(value) => {
                match self.pointer_origins.get(value.index()).copied().flatten() {
                    Some(PointerOrigin::Parameter(place)) => place,
                    Some(PointerOrigin::Address(address)) => {
                        let info = &self.body.addresses[address.index()];
                        let (AddressBase::Slot(slot), 0) = (info.base, info.steps.len) else {
                            return None;
                        };
                        self.body.slots[slot.index()]
                    }
                    _ => return None,
                }
            }
            AddressBase::Address(_) => return None,
        };
        let PlaceTy::Declared(row) = declared else {
            return None;
        };
        let shape = &self.module.types[row.index()];
        if shape.repr != Repr::Own {
            return None;
        }
        let [field] = shape.fields(None)? else {
            return None;
        };
        let FieldTy::Array(element, lanes) = &field.ty else {
            return None;
        };
        let FieldTy::Scalar(lane) = **element else {
            return None;
        };
        lane.size().checked_mul(*lanes)
    }

    /// Fixed projections retain the extent of a typed slot or loan. A raw
    /// offset, reinterpretation or dereference needs its own size proof.
    fn fixed_projection(&self, mut address: AddressId) -> bool {
        loop {
            let info = &self.body.addresses[address.index()];
            let steps = info.steps.get(&self.body.pools.steps).unwrap();
            if steps
                .iter()
                .any(|step| matches!(step, Step::Byte(_) | Step::Index(_) | Step::Deref))
            {
                return false;
            }
            address = match info.base {
                AddressBase::Slot(_) => return true,
                AddressBase::Address(parent) => parent,
                AddressBase::Value(value) if steps.is_empty() => {
                    match self.pointer_origins.get(value.index()).copied().flatten() {
                        Some(PointerOrigin::Parameter(place @ PlaceTy::Declared(t))) => {
                            return info.ty == place
                                && self.module.types[t.index()].repr == Repr::Own;
                        }
                        Some(PointerOrigin::Address(parent))
                            if info.ty == self.body.addresses[parent.index()].ty =>
                        {
                            parent
                        }
                        _ => return false,
                    }
                }
                AddressBase::Value(_) => return false,
            };
        }
    }

    pub(super) fn read_write_alignment(&self, address: AddressId, ty: Ty) -> u32 {
        if !self.facts {
            return 1;
        }
        self.address_alignment(address).min(size(ty)) as u32
    }

    /// The frame allocation proves a slot's alignment. Each fixed offset or
    /// stride preserves only the common power-of-two alignment. Byte
    /// projections, loaded pointers, and borrowed addresses do not carry an
    /// alignment guarantee in CoreIR.
    fn address_alignment(&self, address: AddressId) -> u64 {
        let info = &self.body.addresses[address.index()];
        let place_alignment = |place| match place {
            PlaceTy::Declared(row) if self.module.types[row.index()].repr == Repr::Region => 1,
            _ => self.place_layout(place).1,
        };
        let mut align = match info.base {
            AddressBase::Slot(slot) => place_alignment(self.body.slots[slot.index()]),
            AddressBase::Address(parent) => self.address_alignment(parent),
            AddressBase::Value(value) => {
                match self.pointer_origins.get(value.index()).copied().flatten() {
                    Some(PointerOrigin::Address(parent)) => self.address_alignment(parent),
                    _ => 1,
                }
            }
        };
        for (step_index, step) in info
            .steps
            .get(&self.body.pools.steps)
            .unwrap()
            .iter()
            .enumerate()
        {
            let displacement = match *step {
                Step::Byte(_) | Step::Deref => return 1,
                _ if align == 1 => return 1,
                Step::Seq(word) => word.offset(),
                Step::Index(_) => self.place_layout(info.ty).0,
                Step::Element(_) => {
                    let At::Array(item, _) = self.walk(address, step_index) else {
                        unreachable!("producer array projection")
                    };
                    field_size(&self.module.types, self.recipes, &item).0
                }
                Step::Field(field) | Step::CaseField { field, .. } => {
                    let case = match *step {
                        Step::CaseField { case, .. } => case as usize,
                        _ => 0,
                    };
                    let At::Declared(t) = self.walk(address, step_index) else {
                        unreachable!("producer declared projection")
                    };
                    self.fixed(t).offsets[case][field as usize]
                }
            };
            align = align.min(1u64 << displacement.trailing_zeros().min(63));
        }
        align
    }

    fn kind_at(&self, address: AddressId) -> At {
        self.walk(address, usize::MAX)
    }
    /// The kind of `address`'s place after the first `last` steps of its own
    /// address (all of its bases' steps).
    fn walk(&self, address: AddressId, last: usize) -> At {
        let top = address;
        let mut chain = Vec::new();
        let mut address = address;
        let mut at = loop {
            let info = &self.body.addresses[address.index()];
            chain.push(address);
            match info.base {
                AddressBase::Slot(slot) => {
                    break match self.body.slots[slot.index()] {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    };
                }
                AddressBase::Value(_) => {
                    break match info.ty {
                        PlaceTy::Declared(t) if info.steps.len == 0 => At::Declared(t),
                        _ => At::Other,
                    };
                }
                AddressBase::Address(parent) => address = parent,
            }
        };
        for address in chain.into_iter().rev() {
            let info = &self.body.addresses[address.index()];
            let steps = info.steps.get(&self.body.pools.steps).unwrap();
            let take = if address == top {
                last.min(steps.len())
            } else {
                steps.len()
            };
            for step in &steps[..take] {
                at = match *step {
                    Step::Deref => match at {
                        At::Pointer(t) => At::Declared(t),
                        At::Counted(t) => self.kind_of(t),
                        _ => At::Other,
                    },
                    Step::Index(_) | Step::Byte(_) => match info.ty {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    },
                    Step::Seq(_) => At::Other,
                    Step::Element(_) => {
                        let At::Array(element, _) = at else {
                            unreachable!()
                        };
                        self.field_kind(&element)
                    }
                    Step::Field(field) | Step::CaseField { case: _, field } => {
                        let case = match *step {
                            Step::CaseField { case, .. } => Some(case),
                            _ => None,
                        };
                        let At::Declared(t) = at else { unreachable!() };
                        self.field_kind(
                            &self.module.types[t.index()].fields(case).unwrap()[field as usize].ty,
                        )
                    }
                };
            }
        }
        at
    }
    pub(super) fn project(&mut self, address: AddressId, fail: Option<BlockId>) -> Result<()> {
        let info = &self.body.addresses[address.index()];
        let (mut p, mut at) = match info.base {
            AddressBase::Slot(s) => (
                Value::from(Local::Slot(s.index())),
                match self.body.slots[s.index()] {
                    PlaceTy::Declared(t) => self.kind_of(t),
                    _ => At::Other,
                },
            ),
            AddressBase::Value(v) => (
                self.numeric(v),
                match info.ty {
                    PlaceTy::Declared(t) if info.steps.len == 0 => At::Declared(t),
                    _ => At::Other,
                },
            ),
            AddressBase::Address(a) => (self.address(a), self.kind_at(a)),
        };
        let in_place = self.facts && self.fixed_projection(address);
        for step in info.steps.get(&self.body.pools.steps).unwrap() {
            match *step {
                Step::Deref => {
                    p = self.load(&Type::Ptr, &p, 1);
                    at = match at {
                        At::Pointer(t) => At::Declared(t),
                        At::Counted(t) => {
                            p = self.offset(&p, 8);
                            self.kind_of(t)
                        }
                        _ => At::Other,
                    };
                }
                Step::Seq(w) => {
                    // The typed carrier owns data, length and capacity at
                    // fixed offsets inside its 24-byte representation.
                    p = self.offset_with_bounds(&p, w.offset(), in_place);
                    at = At::Other;
                }
                Step::Byte(v) => {
                    let offset = self.numeric(v);
                    p = self.offset_by(&p, &offset);
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    };
                }
                Step::Index(v) => {
                    let stride = self.place_layout(info.ty).0;
                    let i = self.index_word(v);
                    p = self.gep(&Type::bytes(stride), &p, &i, false);
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    };
                }
                Step::Element(v) => {
                    let At::Array(e, n) = at else {
                        panic!("producer array projection")
                    };
                    let i = self.numeric(v);
                    let outside = self.cmp(Predicate::Uge, &ty(self.vty(v)), &i, &int(n));
                    self.fail_if(&outside, fail, 1)?;
                    let i = self.widen_index(v, i);
                    let stride = field_size(&self.module.types, self.recipes, &e).0;
                    // The successful bounds check proves i < the array extent.
                    // A zero-sized array need not have an allocated object.
                    p = self.gep(&Type::bytes(stride), &p, &i, in_place && stride != 0);
                    at = self.field_kind(&e);
                }
                Step::Field(field) | Step::CaseField { case: _, field } => {
                    let case = match *step {
                        Step::CaseField { case, .. } => Some(case),
                        _ => None,
                    };
                    let At::Declared(t) = at else {
                        panic!("producer declared projection")
                    };
                    let offset = self.fixed(t).offsets[case.unwrap_or(0) as usize][field as usize];
                    // The fixed recipe places this field within the typed value;
                    // the producer has selected the case before projecting it.
                    p = self.offset_with_bounds(&p, offset, in_place);
                    at = self.field_kind(
                        &self.module.types[t.index()].fields(case).unwrap()[field as usize].ty,
                    );
                }
            }
        }
        let local = Local::Address(address.index());
        if self
            .address_slots
            .get(address.index())
            .copied()
            .unwrap_or(true)
        {
            self.store(&Type::Ptr, &p, &local.into(), 0);
        } else {
            self.copy(local, &Type::Ptr, &p);
        }
        Ok(())
    }
    fn index_word(&mut self, v: ValueId) -> Value {
        let i = self.numeric(v);
        self.widen_index(v, i)
    }
    fn widen_index(&mut self, v: ValueId, i: Value) -> Value {
        if size(self.vty(v)) < 8 {
            self.zext(&ty(self.vty(v)), &i, &Type::I64)
        } else {
            i
        }
    }
    fn allocation(&mut self, name: &str, args: &[CArg]) -> (Value, Value) {
        self.allocation_call(name, args, false)
    }
    pub(super) fn allocation_cold(&mut self, name: &str, args: &[CArg]) -> (Value, Value) {
        self.allocation_call(name, args, true)
    }
    fn allocation_call(&mut self, name: &str, args: &[CArg], cold: bool) -> (Value, Value) {
        let grant = Type::structure([Type::Ptr, Type::I64, Type::I64]);
        let p = self.temp(grant.clone());
        let mut full = vec![(Parameter::sret(grant), p.clone())];
        full.extend_from_slice(args);
        let attrs = CallAttrs {
            cold,
            ..CallAttrs::default()
        };
        self.runtime(name, Type::Void, &full, &attrs);
        let pointer = self.load(&Type::Ptr, &p, 8);
        let available = self.offset(&p, 16);
        let available = self.load(&Type::I64, &available, 8);
        (pointer, available)
    }
    /// Admit and allocate one block, returning null and room on refusal. The
    /// caller applies the grant status before continuing the source instruction.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn allocate_raw(
        &mut self,
        kind: &AllocKind,
        place: &Payer,
        charge: &Value,
        size: &Value,
        align: u64,
        overflow: Option<ValueId>,
    ) -> (Value, Value) {
        if matches!(
            kind,
            AllocKind::Own {
                edge: FieldTy::FrozenBytes
            }
        ) {
            return (
                Value::IntToPtr(hirsel_core_layout::FROZEN_STATIC_UNCHARGED),
                int(0),
            );
        }
        // The prefix follows the block's cleanup class (crate docs,
        // "Cleanup classes"): terminal blocks carry only the payer word.
        let native_kind = match kind.cleanup(&self.module.types, self.recipes) {
            Some(hirsel_core_layout::Cleanup::Terminal | hirsel_core_layout::Cleanup::Fixed) => {
                memory_layout::NATIVE_PLAIN
            }
            _ => memory_layout::NATIVE_GRAPH,
        };
        // A fixed block's uncharged spare frame words follow its value.
        let spares = match kind {
            AllocKind::Own { edge } => {
                hirsel_core_layout::spare_words(&self.module.types, self.recipes, edge)
            }
            AllocKind::Environment { ty } => {
                hirsel_core_layout::frames(&self.module.types, self.recipes, *ty).spares
            }
            _ => 0,
        };
        let padded;
        let size = if spares == 0 {
            size
        } else {
            let up = self.bin(Binary::Add, &Type::I64, size, &int(7));
            let up = self.bin(Binary::And, &Type::I64, &up, &int(!7u64));
            padded = self.bin(Binary::Add, &Type::I64, &up, &int(8 * spares));
            &padded
        };
        let resource = match *kind {
            AllocKind::Resource { grant } => Some(grant),
            _ => None,
        };
        let fast = self.fast_allocate(
            resource.is_some(),
            native_kind,
            place,
            charge,
            size,
            align,
            overflow,
        );
        if let Some(fast) = fast {
            return fast;
        }
        let (name, mut args) = match (resource, place) {
            (Some(grant), _) => {
                let without_grant = self.bin(Binary::Sub, &Type::I64, charge, &int(grant));
                let storage = self.bin(Binary::Sub, &Type::I64, &without_grant, &int(32));
                (
                    "hirsel_core_resource_alloc",
                    vec![
                        ptr(CONTEXT),
                        word(size.clone()),
                        word(int(align)),
                        word(storage),
                        word(int(grant)),
                    ],
                )
            }
            (_, Payer::Region(r)) => (
                "hirsel_core_region_slot_alloc",
                vec![
                    ptr(CONTEXT),
                    ptr(r.clone()),
                    word(size.clone()),
                    word(int(align)),
                    word(charge.clone()),
                ],
            ),
            (_, Payer::Current) => (
                "hirsel_core_alloc",
                vec![
                    ptr(CONTEXT),
                    ptr(Value::Null),
                    word(size.clone()),
                    word(int(align)),
                    word(charge.clone()),
                ],
            ),
        };
        if resource.is_none() {
            let overflow = overflow.map_or(FALSE, |v| {
                let value = self.numeric(v);
                self.cmp(Predicate::Ne, &Type::I8, &value, &int(0))
            });
            args.push(zeroext(Type::I1, overflow));
        }
        if resource.is_none() && !matches!(place, Payer::Region(_)) {
            args.push(zeroext(Type::I8, int(native_kind)));
        }
        self.allocation(name, &args)
    }
    pub(super) fn step(&mut self, fail: Option<BlockId>) {
        if self.prepaid_step() {
            return;
        }
        if self.unchecked_steps {
            return;
        }
        let counter = self.step_counter();
        let before = self.load_steps(&counter);
        let after = self.bin(Binary::Sub, &Type::I64, &before, &int(1));
        self.store_steps(&after, &counter);
        let positive = self.cmp(Predicate::Sgt, &Type::I64, &after, &int(0));
        let n = self.counter();
        let ready = Label::new("step_ready", n);
        let slow = Label::new("step_out", n);
        self.likely(&positive, ready, slow);
        self.block(slow);
        let site = self.site_word();
        let status = self.call_declared(
            "hirsel_core_step_out",
            Type::I8,
            &[ptr(CONTEXT), word(site)],
            FnAttrs::default(),
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
        self.branch_failure(&failed, fail, None);
        self.br(ready);
        self.block(ready);
    }
    pub(super) fn bulk_fill(&mut self, fill: hirsel_core_ir::fills::Fill) -> Result<()> {
        // The header proved index < bound. The recognized loop had only its
        // store and unit increment. Its stores cover a single contiguous
        // extent, so the fill can write those bytes before taking the same
        // backedge with bound, leaving the final address and values intact.
        let index = self.numeric(fill.index);
        let bound = self.numeric(fill.bound);
        let base = self.numeric(fill.base);
        let stride = int(fill.stride);
        let start = self.bin(Binary::Mul, &Type::I64, &index, &stride);
        let to = self.offset_by(&base, &start);
        let remaining = self.bin(Binary::Sub, &Type::I64, &bound, &index);
        let bytes = self.bin(Binary::Mul, &Type::I64, &remaining, &stride);
        let byte = fill
            .byte
            .map_or_else(|| int(0), |value| self.numeric(value));
        self.intrinsic(
            "llvm.memset.p0.i64",
            Type::Void,
            &[
                ptr(to),
                arg(Type::I8, byte),
                word(bytes),
                arg(Type::I1, FALSE),
            ],
        );
        let last = self.bin(Binary::Sub, &Type::I64, &bound, &int(1));
        let offset = self.bin(Binary::Mul, &Type::I64, &last, &stride);
        self.put(fill.offset, &offset);
        self.project(fill.store, None)?;
        self.put(fill.next, &bound);
        Ok(())
    }

    pub(super) fn storage(&mut self, inst: &Inst, index: usize) -> Result<()> {
        match *inst {
            Inst::Alloc {
                result,
                available,
                region_entry,
                ref kind,
                place,
                charge,
                overflow,
                size,
                align,
            } => {
                let payer = match place {
                    AllocPlace::Current
                    | AllocPlace::Like(_)
                    | AllocPlace::LentFrame(_)
                    | AllocPlace::ByteHead
                    | AllocPlace::ByteTail(_) => Payer::Current,
                    AllocPlace::Region(r) => Payer::Region(self.numeric(r)),
                };
                let (charge, size) = (self.numeric(charge), self.numeric(size));
                let (block, room) = if let AllocPlace::LentFrame(frame) = place {
                    let frame = self.numeric(frame);
                    let overflow = overflow.map_or(FALSE, |v| {
                        let flag = self.numeric(v);
                        self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
                    });
                    let admitted = self.allocation_cold(
                        "hirsel_core_admit_lent_frame",
                        &[
                            ptr(CONTEXT),
                            ptr(frame),
                            word(size),
                            word(charge),
                            zeroext(Type::I1, overflow),
                        ],
                    );
                    admitted
                } else if matches!(place, AllocPlace::ByteHead | AllocPlace::ByteTail(_)) {
                    let overflow = overflow.map_or(FALSE, |v| {
                        let flag = self.numeric(v);
                        self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
                    });
                    let mut args = vec![ptr(CONTEXT)];
                    let name = if let AllocPlace::ByteTail(head) = place {
                        args.push(ptr(self.numeric(head)));
                        "hirsel_core_alloc_byte_tail"
                    } else {
                        "hirsel_core_alloc_byte_head"
                    };
                    args.extend([word(size), word(charge), zeroext(Type::I1, overflow)]);
                    self.allocation(name, &args)
                } else {
                    self.allocate_raw(kind, &payer, &charge, &size, align, overflow)
                };
                if let Some(token) = region_entry {
                    let Payer::Region(region) = &payer else {
                        unreachable!()
                    };
                    let n = self.counter();
                    let refused = Label::new("entry_refused", n);
                    let initialized = Label::new("entry_initialized", n);
                    let join = Label::new("entry_join", n);
                    let success = self.cmp(Predicate::Ne, &Type::Ptr, &block, &Value::Null);
                    self.likely(&success, initialized, refused);
                    self.block(initialized);
                    let backing = self.load(&Type::Ptr, region, 8);
                    let at = self.offset(&backing, super::memory_layout::REGION_ENTRY as u64);
                    let value = self.load(&Type::I64, &at, 8);
                    self.br(join);
                    self.block(refused);
                    self.br(join);
                    self.block(join);
                    let entry = self.emit(Operation::Phi(
                        &Type::I64,
                        &[(value, initialized), (int(0), refused)],
                    ));
                    self.put(token, &entry);
                }
                self.put(result, &block);
                self.put(available, &room);
                if let Some(counted) = kind.counted() {
                    let missing = self.cmp(Predicate::Eq, &Type::Ptr, &block, &Value::Null);
                    let n = self.counter();
                    let allocated = Label::new("allocated", n);
                    let counted_label = Label::new("counted", n);
                    self.cond(&missing, allocated, counted_label);
                    self.block(counted_label);
                    if counted == Counted::Arc {
                        self.initialize_arc(&block);
                    } else {
                        self.store(&Type::I64, &int(1), &block, 8);
                    }
                    self.br(allocated);
                    self.block(allocated);
                }
                // Word 0 of an environment names its fixed descriptor.
                if let AllocKind::Environment { ty } = kind {
                    let missing = self.cmp(Predicate::Eq, &Type::Ptr, &block, &Value::Null);
                    let n = self.counter();
                    let allocated = Label::new("environment_allocated", n);
                    let named = Label::new("environment_named", n);
                    self.cond(&missing, allocated, named);
                    self.block(named);
                    self.store(
                        &Type::Ptr,
                        &Value::global(format!("core_fixed_env_{}", ty.0)),
                        &block,
                        8,
                    );
                    self.br(allocated);
                    self.block(allocated);
                }
            }
            // Full-new admission and install-before-refund are unchanged.
            // Growth may reuse ordinary local backing under the shared policy;
            // every other buffer takes the original allocate/copy/free path.
            Inst::Grow {
                result,
                available,
                carrier,
                charge,
                overflow,
                size,
                align,
                capacity,
                moved,
                refund,
            } => {
                let edge = hirsel_core_ir::sequence_edge(
                    &self.module.types,
                    self.body.addresses[carrier.index()].ty,
                )
                .expect("wf: grow of a sequence carrier")
                .clone();
                let at = self.address(carrier);
                let old = self.load(&Type::Ptr, &at, 8);
                let (charge, size) = (self.numeric(charge), self.numeric(size));
                let (moved, capacity, refund) = (
                    self.numeric(moved),
                    self.numeric(capacity),
                    self.numeric(refund),
                );
                let (block, room) = self.grow_buffer(
                    edge, &at, &old, &charge, &size, align, overflow, &capacity, &moved, &refund,
                );
                self.put(result, &block);
                self.put(available, &room);
            }
            Inst::Pointer { result, address } => {
                let p = self.address(address);
                self.put(result, &p);
            }
            Inst::Trim {
                region,
                entry,
                previous,
                charge,
                ..
            } => {
                let slot = self.numeric(region);
                let region = self.load(&Type::Ptr, &slot, 8);
                let entry = self.numeric(entry);
                let previous = self.numeric(previous);
                let charge = self.numeric(charge);
                self.runtime_void(
                    "hirsel_core_region_lower_charge",
                    &[ptr(region), word(entry), word(previous), word(charge)],
                );
            }
            Inst::Free { block, charge } => {
                let p = self.numeric(block);
                let n = self.numeric(charge);
                self.free_block(&p, &n);
            }
            Inst::PromoteFrame { carrier } => {
                if let Some(FieldTy::Seq(elem)) = hirsel_core_ir::sequence_edge(
                    &self.module.types,
                    self.body.addresses[carrier.index()].ty,
                ) {
                    let stride =
                        hirsel_core_layout::field_size(&self.module.types, self.recipes, elem).0;
                    let at = self.address(carrier);
                    let data = self.load_native(&at, 0, Type::Ptr);
                    let len = self.load_native(&at, 8, Type::I64);
                    let cap = self.load_native(&at, 16, Type::I64);
                    let promoted = self.intrinsic(
                        "hirsel_core_promote_frame_list",
                        Type::Ptr,
                        &[
                            ptr(CONTEXT),
                            ptr(data),
                            word(cap),
                            word(len),
                            word(int(stride)),
                        ],
                    );
                    self.store_native(&at, 0, Type::Ptr, &promoted);
                }
            }
            Inst::FrameAdmit { .. } | Inst::FrameActive { .. } | Inst::FrameRefund { .. } => {
                self.frame(inst);
            }
            Inst::Copy { to, from, bytes } => {
                let pieces = match self.body.addresses[to.index()].ty {
                    PlaceTy::Declared(t)
                        if self.body.addresses[from.index()].ty == PlaceTy::Declared(t)
                            && matches!(self.recipes[t.index()].layout, Layout::Fixed(_))
                            && !self.by_block(t)
                            && self.constants[bytes.index()] == Some(self.fixed(t).size) =>
                    {
                        self.copy_pieces(t).map(|p| (p, self.fixed(t).align))
                    }
                    _ => None,
                };
                let scopes = self.access_facts(&[to, from]);
                let to = self.address(to);
                let from = self.address(from);
                if let Some((pieces, align)) = pieces {
                    for (at, width) in pieces {
                        let ty = Type::Int(8 * width as u32);
                        let memory = Memory {
                            align: width.min(align) as u32,
                            scopes: scopes.clone(),
                            ..Memory::default()
                        };
                        let (src, dst) = (self.offset(&from, at), self.offset(&to, at));
                        let value = self.load_with(&ty, &src, &memory);
                        self.store_with(&ty, &value, &dst, &memory);
                    }
                    return Ok(());
                }
                // The inline intrinsic has the same nonoverlap contract but
                // requires LLVM to emit the small copy without a library call.
                let (name, n) = match self.constants[bytes.index()] {
                    Some(n @ 1..=16) => ("llvm.memcpy.inline.p0.p0.i64", int(n)),
                    _ => ("llvm.memcpy.p0.p0.i64", self.numeric(bytes)),
                };
                self.runtime(
                    name,
                    Type::Void,
                    &[ptr(to), ptr(from), word(n), arg(Type::I1, FALSE)],
                    &CallAttrs {
                        scopes,
                        ..CallAttrs::default()
                    },
                );
            }
            // No alias scopes: the two places may be the same storage.
            Inst::Move { to, from, bytes } => {
                let (to, from) = (self.address(to), self.address(from));
                let n = self.numeric(bytes);
                self.runtime(
                    "llvm.memmove.p0.p0.i64",
                    Type::Void,
                    &[ptr(to), ptr(from), word(n), arg(Type::I1, FALSE)],
                    &CallAttrs::default(),
                );
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let n = self.numeric(bytes);
                let (yes, room) = if self.empty_bounds.enters.contains(&admitted) {
                    self.empty_bound(&n)
                } else {
                    let (p, room) =
                        self.allocation("hirsel_core_limit_enter", &[ptr(CONTEXT), word(n)]);
                    let yes = self.cmp(Predicate::Ne, &Type::Ptr, &p, &Value::Null);
                    (self.zext(&Type::I1, &yes, &Type::I8), room)
                };
                self.put(admitted, &yes);
                self.put(available, &room);
            }
            Inst::Step { fail } => self.step(fail),
            Inst::LimitExit => {
                if !self.empty_bounds.exits.contains(&index) {
                    self.runtime_void("hirsel_core_limit_exit", &[ptr(CONTEXT)]);
                }
            }
            Inst::RegionOpen { result } => {
                let slot = self.temp(Type::Ptr);
                self.store(&Type::Ptr, &Value::Null, &slot, 8);
                self.put(result, &slot);
            }
            Inst::RegionClose { region } => {
                let slot = self.numeric(region);
                let backing = self.load(&Type::Ptr, &slot, 8);
                let n = self.counter();
                let close = Label::new("region_close", n);
                let done = Label::new("region_closed", n);
                let open = self.cmp(Predicate::Ne, &Type::Ptr, &backing, &Value::Null);
                self.cond(&open, close, done);
                self.block(close);
                self.runtime_void("hirsel_core_region_close", &[ptr(backing)]);
                self.br(done);
                self.block(done);
            }
            Inst::Case { result, address } => {
                let p = self.address(address);
                let PlaceTy::Declared(t) = self.body.addresses[address.index()].ty else {
                    unreachable!()
                };
                let tag = self.fixed(t).tag.unwrap();
                let x = self.case_value(&tag, &p);
                self.put(result, &x);
            }
            Inst::SetCase { address, case } => {
                let p = self.address(address);
                let PlaceTy::Declared(t) = self.body.addresses[address.index()].ty else {
                    unreachable!()
                };
                let tag = self.fixed(t).tag.unwrap();
                self.set_case(&tag, case, &p);
            }
            Inst::Drop { address, ty, fail } => {
                let p = self.address(address);
                // A scalar sequence has no element cleanup. With no finalizer,
                // capacity zero makes its complete drop empty (FORM Compact
                // data). Read it here so the caller's carrier role survives;
                // generic drop glue has no activation-specific alias scope.
                let scopes = self.aliases.as_ref().and_then(|a| a.carrier(address));
                let guard = match scopes {
                    Some(scopes)
                        if hirsel_core_ir::roles::carrier(self.module, ty)
                            && self.module.types[ty.index()].finalizer.is_none() =>
                    {
                        let n = self.counter();
                        let at = self.offset(&p, 16);
                        let memory = Memory {
                            scopes: Some(scopes),
                            ..Memory::aligned(1)
                        };
                        let capacity = self.load_with(&Type::I64, &at, &memory);
                        let owned = self.cmp(Predicate::Ne, &Type::I64, &capacity, &int(0));
                        let drop = Label::new("carrier_drop", n);
                        let done = Label::new("carrier_done", n);
                        self.cond(&owned, drop, done);
                        self.block(drop);
                        Some(done)
                    }
                    _ => None,
                };
                let site = self.site_word();
                // Only the named String append body needs its empty scalar
                // temporaries exposed before the ordinary inliner sees it.
                let always_inline = self.append_fast
                    && hirsel_core_ir::roles::carrier(self.module, ty)
                    && self.module.types[ty.index()].finalizer.is_none();
                let failed = self.call(
                    &super::cleanup::drop_signature(),
                    &Value::global(format!("core_drop_{}", ty.0)),
                    &[CONTEXT, p, site],
                    &CallAttrs {
                        inline: if always_inline {
                            Inline::Always
                        } else {
                            Inline::Default
                        },
                        ..CallAttrs::default()
                    },
                );
                let failed = self.cmp(Predicate::Ne, &Type::I8, &failed, &int(0));
                self.branch_failure(&failed, fail, None);
                if let Some(done) = guard {
                    self.br(done);
                    self.block(done);
                }
            }
            Inst::Share { node, kind } => {
                let p = self.numeric(node);
                self.share_node(&p, kind == Counted::Arc);
            }
            Inst::TryUnwrap { result, node, kind } => {
                let p = self.numeric(node);
                let yes = self.unique_node(&p, kind == Counted::Arc);
                let yes = self.zext(&Type::I1, &yes, &Type::I8);
                self.put(result, &yes);
            }
            Inst::Release { node, kind, ty } => {
                let p = self.numeric(node);
                self.release_node(&p, TypeRef(ty.0), kind == Counted::Arc)?;
            }
            _ => return Err(Error::Unsupported("task operations".into())),
        }
        Ok(())
    }
}
