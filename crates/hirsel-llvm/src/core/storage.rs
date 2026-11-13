//! Recipe-driven storage and runtime memory operations.
use super::*;
use hirsel_core_layout::{FieldTy, Layout, Repr, TypeRef, field_size};

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
    fn kind_of(&self, t: TypeId) -> At {
        if self.module.types[t.index()].repr == Repr::Resource {
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
            PlaceTy::Declared(t) if self.module.types[t.index()].repr == Repr::Resource => (8, 8),
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
                self.get(v),
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
                    let offset = self.get(v);
                    p = self.calc(format!("getelementptr i8, ptr {p}, i64 {offset}"));
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.kind_of(t),
                        _ => At::Other,
                    };
                }
                Step::Index(v) => {
                    let stride = self.place_layout(info.ty).0;
                    let i = self.get(v);
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
                    let i = self.get(v);
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
        self.line(format!("store ptr {p}, ptr %a{}", address.0));
        Ok(())
    }
    pub(super) fn runtime_void(&mut self, name: &str, args: &[(String, String)]) {
        self.intrinsics.insert(format!(
            "declare void @{name}({})",
            args.iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.line(format!(
            "call void @{name}({})",
            args.iter()
                .map(|(t, v)| format!("{t} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
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
        let mut types = vec!["ptr sret({ ptr, i64, i64 })".to_string()];
        types.extend(args.iter().map(|(t, _)| t.clone()));
        self.intrinsics
            .insert(format!("declare void @{name}({})", types.join(", ")));
        self.line(format!(
            "call void @{name}(ptr sret({{ ptr, i64, i64 }}) {p}, {}){attributes}",
            args.iter()
                .map(|(t, v)| format!("{t} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let pointer = self.calc(format!("load ptr, ptr {p}, align 8"));
        let available = self.calc(format!("getelementptr i8, ptr {p}, i64 16"));
        let available = self.calc(format!("load i64, ptr {available}, align 8"));
        (pointer, available)
    }
    pub(super) fn storage(&mut self, inst: &Inst, index: usize) -> Result<()> {
        let ptr = |s: &str| ("ptr".into(), s.to_string());
        let word = |s: String| ("i64".into(), s);
        match *inst {
            Inst::Alloc {
                result,
                available,
                region_entry,
                kind,
                place,
                charge,
                size,
                align,
                fail,
            } => {
                let charge = self.get(charge);
                let size = self.get(size);
                let (block, room) =
                    if let Some(fast) = self.fast_allocate(kind, place, &charge, &size, align) {
                        fast
                    } else {
                        let (name, mut args) = match (kind, place) {
                            (AllocKind::Resource { grant }, _) => {
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
                            (_, AllocPlace::Region(r)) => {
                                let r = self.get(r);
                                (
                                    "hirsel_core_region_alloc",
                                    vec![
                                        ptr("%context"),
                                        ptr(&r),
                                        word(size.to_string()),
                                        word(align.to_string()),
                                        word(charge),
                                    ],
                                )
                            }
                            (_, place) => {
                                let like = match place {
                                    AllocPlace::Like(v) => self.get(v),
                                    _ => "null".into(),
                                };
                                (
                                    "hirsel_core_alloc",
                                    vec![
                                        ptr("%context"),
                                        ptr(&like),
                                        word(size.to_string()),
                                        word(align.to_string()),
                                        word(charge),
                                    ],
                                )
                            }
                        };
                        if !matches!(kind, AllocKind::Resource { .. }) {
                            args.push(("i1 zeroext".into(), "false".into()));
                        }
                        if !matches!(kind, AllocKind::Resource { .. })
                            && !matches!(place, AllocPlace::Region(_))
                        {
                            args.push((
                                "i8 zeroext".into(),
                                u8::from(kind != AllocKind::Plain).to_string(),
                            ));
                        }
                        self.allocation(name, &args)
                    };
                if let Some(token) = region_entry {
                    let AllocPlace::Region(region) = place else {
                        unreachable!()
                    };
                    let region = self.get(region);
                    let at = self.calc(format!(
                        "getelementptr i8, ptr {region}, i64 {}",
                        super::memory_layout::REGION_ENTRY
                    ));
                    let value = self.calc(format!("load i64, ptr {at}, align 8"));
                    self.put(token, &value);
                }
                // Only refusal needs the runtime's active-finalizer decision.
                let present = self.calc(format!("icmp ne ptr {block}, null"));
                let n = self.next;
                self.next += 1;
                self.likely(
                    &present,
                    &format!("allocation_success{n}"),
                    &format!("allocation_status{n}"),
                );
                writeln!(self.code, "allocation_status{n}:").unwrap();
                self.intrinsics
                    .insert("declare i8 @hirsel_core_allocation_status(ptr, ptr)".into());
                let status = self.calc(format!(
                    "call i8 @hirsel_core_allocation_status(ptr %context, ptr {block}) cold"
                ));
                let failed = self.calc(format!("icmp ne i8 {status}, 0"));
                self.branch_failure(&failed, fail, Some(6));
                self.line(format!("br label %allocation_success{n}"));
                writeln!(self.code, "allocation_success{n}:").unwrap();
                self.put(result, &block);
                self.put(available, &room);
                if kind == AllocKind::Counted {
                    // Both count implementations use the same initial word; later operations
                    // select Rc/Arc from their instruction, including shared constructors.
                    let missing = self.calc(format!("icmp eq ptr {block}, null"));
                    let n = self.next;
                    self.next += 1;
                    self.line(format!(
                        "br i1 {missing}, label %allocated{n}, label %counted{n}"
                    ));
                    writeln!(self.code, "counted{n}:").unwrap();
                    self.line(format!("store i64 1, ptr {block}, align 8"));
                    self.line(format!("br label %allocated{n}"));
                    writeln!(self.code, "allocated{n}:").unwrap();
                }
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
                let region = self.get(region);
                let entry = self.get(entry);
                let previous = self.get(previous);
                let charge = self.get(charge);
                self.runtime_void(
                    "hirsel_core_region_lower_charge",
                    &[ptr(&region), word(entry), word(previous), word(charge)],
                );
            }
            Inst::Free { block, charge } => {
                let p = self.get(block);
                let n = self.get(charge);
                self.free_block(&p, &n);
            }
            Inst::Copy { to, from, bytes } => {
                let to = self.address(to);
                let from = self.address(from);
                let n = self.get(bytes);
                self.runtime_void(
                    "llvm.memcpy.p0.p0.i64",
                    &[ptr(&to), ptr(&from), word(n), ("i1".into(), "false".into())],
                );
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let n = self.get(bytes);
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
                let r = self.get(region);
                self.runtime_void("hirsel_core_region_close", &[ptr(&r)]);
            }
            Inst::Case { result, address } => {
                let p = self.address(address);
                let PlaceTy::Declared(t) = self.body.addresses[address.index()].ty else {
                    unreachable!()
                };
                let tag = self.fixed(t).tag.unwrap();
                let x = self.calc(format!("load i{}, ptr {p}, align 1", tag.width * 8));
                let x = if tag.width < 4 {
                    self.calc(format!("zext i{} {x} to i32", tag.width * 8))
                } else {
                    x
                };
                self.put(result, &x);
            }
            Inst::SetCase { address, case } => {
                let p = self.address(address);
                let PlaceTy::Declared(t) = self.body.addresses[address.index()].ty else {
                    unreachable!()
                };
                let tag = self.fixed(t).tag.unwrap();
                self.line(format!("store i{} {case}, ptr {p}, align 1", tag.width * 8));
            }
            Inst::Drop { address, ty, fail } => {
                let p = self.address(address);
                let failed = self.calc(format!(
                    "call i8 @core_drop_{}(ptr %context, ptr {p})",
                    ty.0
                ));
                let failed = self.calc(format!("icmp ne i8 {failed}, 0"));
                self.branch_failure(&failed, fail, None);
            }
            Inst::Share { node, kind } => {
                let p = self.get(node);
                if kind == Counted::Arc {
                    // The provider currently records Arc events inside this entry.
                    self.runtime_void(
                        "hirsel_core_share",
                        &[ptr(&p), ("i1 zeroext".into(), "true".into())],
                    );
                } else {
                    self.share_node(&p, false);
                }
            }
            Inst::TryUnwrap { result, node, kind } => {
                let p = self.get(node);
                let yes = self.intrinsic(
                    "hirsel_core_try_unwrap",
                    "i1",
                    &[
                        ptr(&p),
                        ("i1 zeroext".into(), (kind == Counted::Arc).to_string()),
                    ],
                );
                let yes = self.calc(format!("zext i1 {yes} to i8"));
                self.put(result, &yes);
            }
            Inst::Release { node, kind, ty } => {
                let p = self.get(node);
                self.release_node(&p, TypeRef(ty.0), kind == Counted::Arc)?;
            }
            _ => return Err(Error::Unsupported("task operations".into())),
        }
        Ok(())
    }
}
