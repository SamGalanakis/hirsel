//! Recipe-driven storage and runtime memory operations.
use super::*;
use hirsel_core_layout::{FieldTy, Layout, Repr, TypeRef, field_size};

/// Where an allocation is charged, resolved to operands.
pub(super) enum Payer {
    Current,
    /// Like the block at this pointer (null charges the current limit).
    Like(String),
    Region(String),
}

#[derive(Clone)]
pub(super) enum At {
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(Box<FieldTy>, u64),
    Other,
}
impl<S: sink::Sink> FunctionEmitter<'_, S> {
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
                format!("%s{}", s.0),
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
                    p = self.calc(format!("load ptr, ptr {p}, align 1"));
                    at = match at {
                        At::Pointer(t) => At::Declared(t),
                        At::Counted(t) => {
                            p = self.calc(format!("getelementptr i8, ptr {p}, i64 8"));
                            self.kind_of(t)
                        }
                        _ => At::Other,
                    };
                }
                Step::Byte(v) => {
                    let offset = self.numeric(v);
                    p = self.calc(format!("getelementptr i8, ptr {p}, i64 {offset}"));
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    };
                }
                Step::Index(v) => {
                    let stride = self.place_layout(info.ty).0;
                    let i = self.numeric(v);
                    let i = if size(self.vty(v)) < 8 {
                        self.calc(format!("zext {} {i} to i64", ty(self.vty(v))))
                    } else {
                        i
                    };
                    p = self.calc(format!("getelementptr [{stride} x i8], ptr {p}, i64 {i}"));
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
                    let outside = self.calc(format!("icmp uge {} {i}, {n}", ty(self.vty(v))));
                    self.fail_if(&outside, fail, 1)?;
                    let i = if size(self.vty(v)) < 8 {
                        self.calc(format!("zext {} {i} to i64", ty(self.vty(v))))
                    } else {
                        i
                    };
                    let stride = field_size(&self.module.types, self.recipes, &e).0;
                    p = self.calc(format!("getelementptr [{stride} x i8], ptr {p}, i64 {i}"));
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
                    p = self.calc(format!("getelementptr i8, ptr {p}, i64 {offset}"));
                    at = self.field_kind(
                        &self.module.types[t.index()].fields(case).unwrap()[field as usize].ty,
                    );
                }
            }
        }
        if self
            .address_slots
            .get(address.index())
            .copied()
            .unwrap_or(true)
        {
            self.line(format!("store ptr {p}, ptr %a{}", address.0));
        } else {
            self.line(format!(
                "%a{} = select i1 true, ptr {p}, ptr poison",
                address.0
            ));
        }
        Ok(())
    }
    pub(super) fn runtime_void(&mut self, name: &str, args: &[(String, String)]) {
        self.runtime_void_metadata(name, args, "");
    }
    fn runtime_void_metadata(&mut self, name: &str, args: &[(String, String)], metadata: &str) {
        let _ = metadata;
        let params: Vec<_> = args.iter().map(|(t, _)| sink::Parameter::plain(super::ir_type_text(self.c_argument(t)))).collect();
        let values: Vec<_> = args.iter().map(|(_, v)| self.operand(v)).collect();
        let signature = sink::Signature { result: sink::Type::Void, extension: sink::Extension::None, params };
        self.sink.instruction(None, sink::Operation::Call(&signature, &sink::Value::Global(name.to_owned()), &values, &sink::CallAttrs::default()), self.debug_location);
    }
    fn allocation(&mut self, name: &str, args: &[(String, String)]) -> (String, String) {
        self.allocation_call(name, args, "")
    }
    pub(super) fn allocation_cold(
        &mut self,
        name: &str,
        args: &[(String, String)],
    ) -> (String, String) {
        self.allocation_call(name, args, " cold")
    }
    fn allocation_call(
        &mut self,
        name: &str,
        args: &[(String, String)],
        attributes: &str,
    ) -> (String, String) {
        let p = self.temp("{ ptr, i64, i64 }");
        let mut params = vec![sink::Parameter { ty: sink::Type::Ptr, extension: sink::Extension::None, noalias: false, readonly: false, sret: Some(sink::Type::structure(vec![sink::Type::Ptr, sink::Type::I64, sink::Type::I64])), byval: None }];
        params.extend(args.iter().map(|(t, _)| sink::Parameter::plain(super::ir_type_text(self.c_argument(t)))));
        let mut values = vec![self.operand(&p)];
        values.extend(args.iter().map(|(_, v)| self.operand(v)));
        let signature = sink::Signature { result: sink::Type::Void, extension: sink::Extension::None, params };
        let _ = attributes;
        self.sink.instruction(None, sink::Operation::Call(&signature, &sink::Value::Global(name.to_owned()), &values, &sink::CallAttrs::default()), self.debug_location);
        let pointer = self.typed(sink::Operation::Load(sink::Type::Ptr, &self.operand(&p), &sink::Memory { align: 8, ..Default::default() }));
        let at = self.typed(sink::Operation::Gep(sink::Type::I8, &self.operand(&p), &[(sink::Type::I64, sink::Value::Integer(16))]));
        let available = self.typed(sink::Operation::Load(sink::Type::I64, &self.operand(&at), &sink::Memory { align: 8, ..Default::default() }));
        (pointer, available)
    }
    /// Admit and allocate one block, returning null and room on refusal. The
    /// caller applies the grant status before continuing the source instruction.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn allocate_raw(
        &mut self,
        kind: &AllocKind,
        place: &Payer,
        charge: &str,
        size: &str,
        align: u64,
        overflow: Option<ValueId>,
        owned_like: bool,
    ) -> (String, String) {
        let ptr = |s: &str| ("ptr".into(), s.to_string());
        let word = |s: String| ("i64".into(), s);
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
        let (block, room) = if let Some(fast) = fast {
            fast
        } else {
            let (name, mut args) = match (resource, place) {
                (Some(grant), _) => {
                    let without_grant = self.calc(format!("sub i64 {charge}, {grant}"));
                    let storage = self.calc(format!("sub i64 {without_grant}, 32"));
                    (
                        "hirsel_core_resource_alloc",
                        vec![
                            ptr("%context"),
                            word(size.to_string()),
                            word(align.to_string()),
                            word(storage),
                            word(grant.to_string()),
                        ],
                    )
                }
                (_, Payer::Region(r)) => (
                    "hirsel_core_region_alloc",
                    vec![
                        ptr("%context"),
                        ptr(r),
                        word(size.to_string()),
                        word(align.to_string()),
                        word(charge.to_string()),
                    ],
                ),
                (_, place) => {
                    let like = match place {
                        Payer::Like(old) => old.clone(),
                        _ => "null".into(),
                    };
                    (
                        "hirsel_core_alloc",
                        vec![
                            ptr("%context"),
                            ptr(&like),
                            word(size.to_string()),
                            word(align.to_string()),
                            word(charge.to_string()),
                        ],
                    )
                }
            };
            if resource.is_none() {
                let overflow = overflow.map_or_else(
                    || "false".into(),
                    |v| {
                        let value = self.numeric(v);
                        self.calc(format!("icmp ne i8 {value}, 0"))
                    },
                );
                args.push(("i1 zeroext".into(), overflow));
            }
            if resource.is_none() && !matches!(place, Payer::Region(_)) {
                args.push(("i8 zeroext".into(), native_kind.to_string()));
            }
            self.allocation(name, &args)
        };
        (block, room)
    }
    #[allow(clippy::too_many_arguments)]
    fn allocate(
        &mut self,
        kind: &AllocKind,
        place: &Payer,
        charge: &str,
        size: &str,
        align: u64,
        overflow: Option<ValueId>,
        fail: Option<BlockId>,
    ) -> (String, String) {
        let (block, room) = self.allocate_raw(kind, place, charge, size, align, overflow, false);
        self.allocation_status(&block, fail);
        (block, room)
    }
    pub(super) fn allocation_status(&mut self, block: &str, fail: Option<BlockId>) {
        // Only refusal needs the runtime's active-finalizer decision.
        let present = self.calc(format!("icmp ne ptr {block}, null"));
        let n = self.next;
        self.next += 1;
        self.likely(
            &present,
            &format!("allocation_success{n}"),
            &format!("allocation_status{n}"),
        );
        self.sink.block(&format!("allocation_status{n}:").trim_end_matches(':'));
        self.intrinsics
            .insert("declare i8 @hirsel_core_allocation_status(ptr, ptr)".into());
        let status = self.calc(format!(
            "call i8 @hirsel_core_allocation_status(ptr %context, ptr {block}) cold"
        ));
        let failed = self.calc(format!("icmp ne i8 {status}, 0"));
        self.branch_failure(&failed, fail, Some(6));
        self.line(format!("br label %allocation_success{n}"));
        self.sink.block(&format!("allocation_success{n}:").trim_end_matches(':'));
    }
    pub(super) fn storage(&mut self, inst: &Inst, index: usize) -> Result<()> {
        let ptr = |s: &str| ("ptr".into(), s.to_string());
        let word = |s: String| ("i64".into(), s);
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
                    let at = self.calc(format!(
                        "getelementptr i8, ptr {region}, i64 {}",
                        super::memory_layout::REGION_ENTRY
                    ));
                    let value = self.calc(format!("load i64, ptr {at}, align 8"));
                    self.put(token, &value);
                }
                self.put(result, &block);
                self.put(available, &room);
                if let Some(counted) = kind.counted() {
                    let missing = self.calc(format!("icmp eq ptr {block}, null"));
                    let n = self.next;
                    self.next += 1;
                    self.line(format!(
                        "br i1 {missing}, label %allocated{n}, label %counted{n}"
                    ));
                    self.sink.block(&format!("counted{n}:").trim_end_matches(':'));
                    if counted == Counted::Arc {
                        self.line(format!(
                            "store atomic i64 1, ptr {block} monotonic, align 8"
                        ));
                        self.arc_event("hirsel_core_arc_initialized", &[]);
                    } else {
                        self.line(format!("store i64 1, ptr {block}, align 8"));
                    }
                    self.line(format!("br label %allocated{n}"));
                    self.sink.block(&format!("allocated{n}:").trim_end_matches(':'));
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
                let old = self.calc(format!("load ptr, ptr {at}, align 8"));
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
                    &[ptr(&region), word(entry), word(previous), word(charge)],
                );
            }
            Inst::Free { block, charge } => {
                let p = self.numeric(block);
                let n = self.numeric(charge);
                self.free_block(&p, &n);
            }
            Inst::Copy { to, from, bytes } => {
                let metadata = self.access_facts(&[to, from]);
                let to = self.address(to);
                let from = self.address(from);
                let n = self.numeric(bytes);
                self.runtime_void_metadata(
                    "llvm.memcpy.p0.p0.i64",
                    &[ptr(&to), ptr(&from), word(n), ("i1".into(), "false".into())],
                    &metadata,
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
                        self.allocation("hirsel_core_limit_enter", &[ptr("%context"), word(n)]);
                    let yes = self.calc(format!("icmp ne ptr {p}, null"));
                    (self.calc(format!("zext i1 {yes} to i8")), room)
                };
                self.put(admitted, &yes);
                self.put(available, &room);
            }
            Inst::LimitExit => {
                if !self.empty_bounds.exits.contains(&index) {
                    self.runtime_void("hirsel_core_limit_exit", &[ptr("%context")]);
                }
            }
            Inst::RegionOpen { result } => {
                let p = self.intrinsic("hirsel_core_region_open", "ptr", &[word("65536".into())]);
                self.put(result, &p);
            }
            Inst::RegionClose { region } => {
                let r = self.numeric(region);
                self.runtime_void("hirsel_core_region_close", &[ptr(&r)]);
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
                let metadata = self
                    .aliases
                    .as_ref()
                    .map_or_else(String::new, |a| a.carrier(address));
                let guard = if !metadata.is_empty()
                    && hirsel_core_ir::roles::carrier(self.module, ty)
                    && self.module.types[ty.index()].finalizer.is_none()
                {
                    let n = self.next;
                    self.next += 1;
                    let at = self.calc(format!("getelementptr i8, ptr {p}, i64 16"));
                    let capacity = self.calc(format!("load i64, ptr {at}, align 1{metadata}"));
                    let owned = self.calc(format!("icmp ne i64 {capacity}, 0"));
                    self.line(format!(
                        "br i1 {owned}, label %carrier_drop{n}, label %carrier_done{n}"
                    ));
                    self.sink.block(&format!("carrier_drop{n}:").trim_end_matches(':'));
                    Some(n)
                } else {
                    None
                };
                let site = self.site_word();
                // Only the named String append body needs its empty scalar
                // temporaries exposed before the ordinary inliner sees it.
                let inline = if self.append_fast
                    && hirsel_core_ir::roles::carrier(self.module, ty)
                    && self.module.types[ty.index()].finalizer.is_none()
                {
                    " alwaysinline"
                } else {
                    ""
                };
                let failed = self.calc(format!(
                    "call i8 @core_drop_{}(ptr %context, ptr {p}, i64 {site}){inline}",
                    ty.0
                ));
                let failed = self.calc(format!("icmp ne i8 {failed}, 0"));
                self.branch_failure(&failed, fail, None);
                if let Some(n) = guard {
                    self.line(format!("br label %carrier_done{n}"));
                    self.sink.block(&format!("carrier_done{n}:").trim_end_matches(':'));
                }
            }
            Inst::Share { node, kind } => {
                let p = self.numeric(node);
                self.share_node(&p, kind == Counted::Arc);
            }
            Inst::TryUnwrap { result, node, kind } => {
                let p = self.numeric(node);
                let yes = self.unique_node(&p, kind == Counted::Arc);
                let yes = self.calc(format!("zext i1 {yes} to i8"));
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
