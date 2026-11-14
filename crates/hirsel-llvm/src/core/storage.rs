//! Recipe-driven storage and runtime memory operations.
use super::*;
use hirsel_core_layout::{FieldTy, Layout, Repr, TypeRef, field_size};

/// A C argument the x86-64 ABI zero-extends.
pub(super) fn zeroext(ty: Type, value: impl Into<Value>) -> CArg {
    (Parameter::extended(ty, Extension::Zero), value.into())
}

/// Where an allocation is charged, resolved to operands.
pub(super) enum Payer {
    Current,
    /// Like the block at this pointer (null charges the current limit).
    Like(Value),
    Region(Value),
}

#[derive(Clone)]
pub(super) enum At {
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(Box<FieldTy>, u64),
    Other,
}
impl FunctionEmitter<'_> {
    pub(super) fn fixed(&self, t: TypeId) -> &hirsel_core_layout::Fixed {
        let Layout::Fixed(f) = &self.recipes[t.index()].layout else {
            panic!("compact projection requires its extent recipe")
        };
        f
    }
    /// A resource or stable value is held as a pointer to its block (MEM-5, MEM-24).
    fn by_block(&self, t: TypeId) -> bool {
        matches!(
            self.module.types[t.index()].repr,
            Repr::Resource | Repr::Stable
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
            FieldTy::Rc(t) | FieldTy::Arc(t) => At::Counted(TypeId(t.0)),
            FieldTy::Array(e, n) => At::Array(e.clone(), *n),
            _ => At::Other,
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
    fn kind_at(&self, address: AddressId) -> At {
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
            for step in info.steps.get(&self.body.pools.steps).unwrap() {
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
                    p = self.gep(&Type::bytes(stride), &p, &i, false);
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
                    p = self.offset(&p, offset);
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
            self.emit_to(
                local,
                Operation::Select(&Type::Ptr, &true.into(), &p, &Value::Poison),
            );
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
    /// A runtime entry with C argument extensions for this target.
    pub(super) fn runtime(
        &mut self,
        name: &str,
        result: Type,
        args: &[CArg],
        attrs: &CallAttrs,
    ) -> Value {
        let args: Vec<CArg> = args
            .iter()
            .map(|(p, v)| (self.c_param(p.clone()), v.clone()))
            .collect();
        self.call_declared(name, result, &args, FnAttrs::default(), attrs)
    }
    pub(super) fn runtime_void(&mut self, name: &str, args: &[CArg]) {
        self.runtime(name, Type::Void, args, &CallAttrs::default());
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
        owned_like: bool,
    ) -> (Value, Value) {
        // The prefix follows the block's cleanup class (crate docs,
        // "Cleanup classes"): terminal blocks carry only the payer word.
        let native_kind = match kind.cleanup(&self.module.types, self.recipes) {
            Some(hirsel_core_layout::Cleanup::Terminal) => memory_layout::NATIVE_PLAIN,
            Some(hirsel_core_layout::Cleanup::Fixed) => memory_layout::NATIVE_STATIC,
            _ => memory_layout::NATIVE_GRAPH,
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
            owned_like,
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
                "hirsel_core_region_alloc",
                vec![
                    ptr(CONTEXT),
                    ptr(r.clone()),
                    word(size.clone()),
                    word(int(align)),
                    word(charge.clone()),
                ],
            ),
            (_, place) => {
                let like = match place {
                    Payer::Like(old) => old.clone(),
                    _ => Value::Null,
                };
                (
                    "hirsel_core_alloc",
                    vec![
                        ptr(CONTEXT),
                        ptr(like),
                        word(size.clone()),
                        word(int(align)),
                        word(charge.clone()),
                    ],
                )
            }
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
    #[allow(clippy::too_many_arguments)]
    fn allocate(
        &mut self,
        kind: &AllocKind,
        place: &Payer,
        charge: &Value,
        size: &Value,
        align: u64,
        overflow: Option<ValueId>,
        fail: Option<BlockId>,
    ) -> (Value, Value) {
        let (block, room) = self.allocate_raw(kind, place, charge, size, align, overflow, false);
        self.allocation_status(&block, fail);
        (block, room)
    }
    pub(super) fn allocation_status(&mut self, block: &Value, fail: Option<BlockId>) {
        // Only refusal needs the runtime's active-finalizer decision.
        let present = self.cmp(Predicate::Ne, &Type::Ptr, block, &Value::Null);
        let n = self.counter();
        let success = Label::new("allocation_success", n);
        let status_label = Label::new("allocation_status", n);
        self.likely(&present, success, status_label);
        self.block(status_label);
        let status = self.call_declared(
            "hirsel_core_allocation_status",
            Type::I8,
            &[ptr(CONTEXT), ptr(block.clone())],
            FnAttrs::default(),
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
        self.branch_failure(&failed, fail, Some(6));
        self.br(success);
        self.block(success);
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
                fail,
            } => {
                let payer = match place {
                    AllocPlace::Current => Payer::Current,
                    AllocPlace::Like(v) => Payer::Like(self.numeric(v)),
                    AllocPlace::Region(r) => Payer::Region(self.numeric(r)),
                };
                let (charge, size) = (self.numeric(charge), self.numeric(size));
                let (block, room) =
                    self.allocate(kind, &payer, &charge, &size, align, overflow, fail);
                if let Some(token) = region_entry {
                    let Payer::Region(region) = &payer else {
                        unreachable!()
                    };
                    let at = self.offset(region, super::memory_layout::REGION_ENTRY as u64);
                    let value = self.load(&Type::I64, &at, 8);
                    self.put(token, &value);
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
                        let memory = Memory {
                            ordering: Some(Ordering::Monotonic),
                            ..Memory::aligned(8)
                        };
                        self.store_with(&Type::I64, &int(1), &block, &memory);
                        self.arc_event("hirsel_core_arc_initialized", &[]);
                    } else {
                        self.store(&Type::I64, &int(1), &block, 8);
                    }
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
                fail,
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
                    edge, &at, &old, &charge, &size, align, overflow, fail, &capacity, &moved,
                    &refund,
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
                let region = self.numeric(region);
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
            Inst::Copy { to, from, bytes } => {
                let scopes = self.access_facts(&[to, from]);
                let to = self.address(to);
                let from = self.address(from);
                let n = self.numeric(bytes);
                self.runtime(
                    "llvm.memcpy.p0.p0.i64",
                    Type::Void,
                    &[ptr(to), ptr(from), word(n), arg(Type::I1, FALSE)],
                    &CallAttrs {
                        scopes,
                        ..CallAttrs::default()
                    },
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
            Inst::LimitExit => {
                if !self.empty_bounds.exits.contains(&index) {
                    self.runtime_void("hirsel_core_limit_exit", &[ptr(CONTEXT)]);
                }
            }
            Inst::RegionOpen { result } => {
                let p = self.intrinsic("hirsel_core_region_open", Type::Ptr, &[word(int(65536))]);
                self.put(result, &p);
            }
            Inst::RegionClose { region } => {
                let r = self.numeric(region);
                self.runtime_void("hirsel_core_region_close", &[ptr(r)]);
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
                    &FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr, Type::I64]),
                    &Value::global(format!("core_drop_{}", ty.0)),
                    &[CONTEXT, p, site],
                    &CallAttrs {
                        always_inline,
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
