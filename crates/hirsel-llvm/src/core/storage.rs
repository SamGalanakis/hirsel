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
            AddressBase::Address(a) => (self.address(a), self.address_kinds[a.index()].clone()),
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
                Step::Index(v) => {
                    let stride = self.place_layout(info.ty).0;
                    let i = self.get(v);
                    let i = if size(self.vty(v)) < 8 {
                        self.calc(format!("zext {} {i} to i64", ty(self.vty(v))))
                    } else {
                        i
                    };
                    let off = self.calc(format!("mul i64 {i}, {stride}"));
                    p = self.calc(format!("getelementptr i8, ptr {p}, i64 {off}"));
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
                    let off = self.calc(format!("mul i64 {i}, {stride}"));
                    p = self.calc(format!("getelementptr i8, ptr {p}, i64 {off}"));
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
        self.address_kinds[address.index()] = at;
        self.line(format!("store ptr {p}, ptr %a{}", address.0));
        Ok(())
    }
    fn runtime_void(&mut self, name: &str, args: &[(String, String)]) {
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
        let p = self.temp("{ ptr, i64, i64 }");
        let mut types = vec!["ptr sret({ ptr, i64, i64 })".to_string()];
        types.extend(args.iter().map(|(t, _)| t.clone()));
        self.intrinsics
            .insert(format!("declare void @{name}({})", types.join(", ")));
        self.line(format!(
            "call void @{name}(ptr sret({{ ptr, i64, i64 }}) {p}, {})",
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
    pub(super) fn storage(&mut self, inst: &Inst) -> Result<()> {
        let ptr = |s: &str| ("ptr".into(), s.to_string());
        let word = |s: String| ("i64".into(), s);
        match *inst {
            Inst::Alloc {
                result,
                available,
                kind,
                place,
                charge,
                size,
                align,
                fail,
            } => {
                let charge = self.get(charge);
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
                let (block, room) = self.allocation(name, &args);
                // Null is an ordinary Capacity value outside a finalizer. The runtime
                // reports whether this context is executing a finalizer grant.
                self.intrinsics
                    .insert("declare i8 @hirsel_core_allocation_status(ptr, ptr)".into());
                let status = self.calc(format!(
                    "call i8 @hirsel_core_allocation_status(ptr %context, ptr {block})"
                ));
                let failed = self.calc(format!("icmp ne i8 {status}, 0"));
                self.branch_failure(&failed, fail, Some(6));
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
                    self.runtime_void(
                        "hirsel_core_counted_init",
                        &[ptr(&block), ("i1 zeroext".into(), "false".into())],
                    );
                    self.line(format!("br label %allocated{n}"));
                    writeln!(self.code, "allocated{n}:").unwrap();
                }
            }
            Inst::Free { block, charge } => {
                let p = self.get(block);
                let n = self.get(charge);
                self.runtime_void("hirsel_core_free_value", &[ptr(&p), word(n)]);
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
                let (p, room) =
                    self.allocation("hirsel_core_limit_enter", &[ptr("%context"), word(n)]);
                let yes = self.calc(format!("icmp ne ptr {p}, null"));
                let yes = self.calc(format!("zext i1 {yes} to i8"));
                self.put(admitted, &yes);
                self.put(available, &room);
            }
            Inst::LimitExit => self.runtime_void("hirsel_core_limit_exit", &[ptr("%context")]),
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
                self.runtime_void(
                    "hirsel_core_share",
                    &[
                        ptr(&p),
                        ("i1 zeroext".into(), (kind == Counted::Arc).to_string()),
                    ],
                );
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

fn cleanup_dependencies(field: &FieldTy, counted: bool, pending: &mut Vec<(usize, bool)>) {
    match field {
        FieldTy::Inline(t) | FieldTy::Box(t) => pending.push((t.index(), counted)),
        FieldTy::Rc(t) | FieldTy::Arc(t) => pending.push((t.index(), true)),
        FieldTy::Array(element, _) => cleanup_dependencies(element, counted, pending),
        _ => {}
    }
}

pub(super) fn drop_glue(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    text: &mut String,
    intrinsics: &mut BTreeSet<String>,
) -> Result<()> {
    let mut normal = BTreeSet::new();
    let mut counted = BTreeSet::new();
    let mut pending = Vec::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            match inst {
                Inst::Drop { ty, .. } => pending.push((ty.index(), false)),
                Inst::Release { ty, .. } => pending.push((ty.index(), true)),
                _ => {}
            }
        }
    }
    while let Some((t, mechanical)) = pending.pop() {
        let seen = if mechanical {
            &mut counted
        } else {
            &mut normal
        };
        if !seen.insert(t) {
            continue;
        }
        if let Some(drop) = &recipes[t].drop {
            for field in drop.fields.iter().flatten() {
                cleanup_dependencies(&field.ty, mechanical, &mut pending);
            }
        }
    }
    let empty = Body::default();
    for t in normal {
        let mut e = FunctionEmitter {
            module,
            body: &empty,
            recipes,
            address_kinds: vec![],
            abi: abi::Return::internal(&[]),
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics,
        };
        let status = e.temp("i8");
        e.line(format!("store i8 0, ptr {status}"));
        let resource = module.types[t].repr == Repr::Resource;
        let base = if resource {
            e.calc("load ptr, ptr %payload, align 1")
        } else {
            "%payload".into()
        };
        if resource {
            #[cfg(test)]
            if e.fixed(TypeId(t as u32)).size >= 8 {
                let id = e.calc(format!("load i64, ptr {base}, align 1"));
                e.runtime_void("hirsel_core_test_drop_trace", &[("i64".into(), id)]);
            }
            if let Some(f) = module.types[t].finalizer {
                e.runtime_void(
                    "hirsel_core_finalizer_begin",
                    &[
                        ("ptr".into(), "%context".into()),
                        ("ptr".into(), base.clone()),
                    ],
                );
                let failed = e.calc(format!(
                    "call i8 @{}(ptr %context, ptr {base})",
                    symbol(module, FuncId(f.0))
                ));
                e.line(format!("store i8 {failed}, ptr {status}"));
                e.runtime_void(
                    "hirsel_core_finalizer_end",
                    &[
                        ("ptr".into(), "%context".into()),
                        ("ptr".into(), base.clone()),
                    ],
                );
            }
        }
        if let Some(drop) = &recipes[t].drop {
            let tag = e.fixed(TypeId(t as u32)).tag;
            if let Some(tag) = tag {
                let which = e.calc(format!("load i{}, ptr {base}, align 1", tag.width * 8));
                e.line(format!("switch i{} {which}, label %case0 [", tag.width * 8));
                for row in 1..drop.fields.len() {
                    e.line(format!("i{} {row}, label %case{row}", tag.width * 8));
                }
                e.line("]");
            } else {
                e.line("br label %case0");
            }
            for (row, fields) in drop.fields.iter().enumerate() {
                writeln!(e.code, "case{row}:").unwrap();
                for field in fields {
                    let p = e.calc(format!(
                        "getelementptr i8, ptr {base}, i64 {}",
                        field.offset
                    ));
                    e.clean_field(&field.ty, &p, &status)?;
                }
                e.line("br label %settle");
            }
            writeln!(e.code, "settle:").unwrap();
        }
        if resource {
            let storage = recipes[t].logical_size.expect("resource storage");
            e.runtime_void(
                "hirsel_core_resource_free",
                &[("ptr".into(), base), ("i64".into(), storage.to_string())],
            );
        }
        let failed = e.calc(format!("load i8, ptr {status}"));
        e.line(format!("ret i8 {failed}"));
        writeln!(
            text,
            "define private i8 @core_drop_{t}(ptr %context, ptr %payload) {{\nentry:\n{}{} }}",
            e.allocations, e.code
        )
        .unwrap();
    }
    counted_glue(module, recipes, &counted, text, intrinsics)?;
    Ok(())
}

impl FunctionEmitter<'_> {
    fn clean_field(&mut self, field: &FieldTy, p: &str, status: &str) -> Result<()> {
        let ptr = |s: &str| ("ptr".into(), s.to_string());
        let failed = match field {
            FieldTy::Inline(t) => {
                Some(self.calc(format!("call i8 @core_drop_{}(ptr %context, ptr {p})", t.0)))
            }
            FieldTy::Box(t) => {
                let block = self.calc(format!("load ptr, ptr {p}, align 1"));
                let failed = self.calc(format!(
                    "call i8 @core_drop_{}(ptr %context, ptr {block})",
                    t.0
                ));
                let logical = if self.module.types[t.index()].repr == Repr::Resource {
                    8
                } else {
                    self.recipes[t.index()].logical_size.unwrap()
                };
                let charge = hirsel_core_layout::charge::boxed(logical).unwrap();
                self.runtime_void(
                    "hirsel_core_free",
                    &[ptr(&block), ("i64".into(), charge.to_string())],
                );
                Some(failed)
            }
            FieldTy::Array(e, n) => {
                let stride = field_size(&self.module.types, self.recipes, e).0;
                let index = self.temp("i64");
                self.line(format!("store i64 0, ptr {index}"));
                let label = self.next;
                self.next += 1;
                self.line(format!("br label %array_head{label}"));
                writeln!(self.code, "array_head{label}:").unwrap();
                let i = self.calc(format!("load i64, ptr {index}"));
                let more = self.calc(format!("icmp ult i64 {i}, {n}"));
                self.line(format!(
                    "br i1 {more}, label %array_body{label}, label %array_done{label}"
                ));
                writeln!(self.code, "array_body{label}:").unwrap();
                let off = self.calc(format!("mul i64 {i}, {stride}"));
                let element = self.calc(format!("getelementptr i8, ptr {p}, i64 {off}"));
                self.clean_field(e, &element, status)?;
                let next = self.calc(format!("add i64 {i}, 1"));
                self.line(format!("store i64 {next}, ptr {index}"));
                self.line(format!("br label %array_head{label}"));
                writeln!(self.code, "array_done{label}:").unwrap();
                None
            }
            FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.calc(format!("load ptr, ptr {p}, align 1"));
                self.release_node(&node, *t, matches!(field, FieldTy::Arc(_)))?;
                None
            }
            FieldTy::Scalar(_) | FieldTy::Stable(_) | FieldTy::Fn | FieldTy::Borrow => None,
            FieldTy::Handle(hirsel_core_layout::Handle::Causes) => {
                let chain = self.calc(format!("load ptr, ptr {p}, align 1"));
                self.runtime_void("hirsel_core_causes_drop", &[ptr(&chain)]);
                None
            }
            other => return Err(Error::Unsupported(format!("drop field {other:?}"))),
        };
        if let Some(failed) = failed {
            let old = self.calc(format!("load i8, ptr {status}"));
            let both = self.calc(format!("or i8 {old}, {failed}"));
            self.line(format!("store i8 {both}, ptr {status}"));
        }
        Ok(())
    }
    fn release_node(&mut self, node: &str, t: TypeRef, atomic: bool) -> Result<()> {
        let logical = self.recipes[t.index()].logical_size.unwrap();
        let charge = hirsel_core_layout::charge::counted(logical).unwrap();
        self.runtime_void(
            "hirsel_core_release",
            &[
                ("ptr".into(), "%context".into()),
                ("ptr".into(), node.into()),
                ("i1 zeroext".into(), atomic.to_string()),
                ("i64".into(), charge.to_string()),
                ("ptr".into(), format!("@core_counted_drop_{}", t.0)),
            ],
        );
        Ok(())
    }
}

fn mechanical_count(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    field: &FieldTy,
) -> Result<u64> {
    Ok(match field {
        FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) => 1,
        FieldTy::Inline(t) => {
            let Some(drop) = &recipes[t.index()].drop else {
                return Ok(0);
            };
            let mut max = 0;
            for row in &drop.fields {
                let mut n = 0;
                for f in row {
                    n += mechanical_count(module, recipes, &f.ty)?;
                }
                max = max.max(n);
            }
            max
        }
        FieldTy::Array(e, n) => mechanical_count(module, recipes, e)? * n,
        FieldTy::Scalar(_) | FieldTy::Fn | FieldTy::Stable(_) => 0,
        other => {
            return Err(Error::Unsupported(format!(
                "counted payload field {other:?}"
            )));
        }
    })
}
impl FunctionEmitter<'_> {
    fn mechanical_field(
        &mut self,
        field: &FieldTy,
        p: &str,
        cursor: &str,
        base: u64,
    ) -> Result<()> {
        let count = mechanical_count(self.module, self.recipes, field)?;
        if count == 0 {
            return Ok(());
        }
        let local = self.calc(format!("sub i64 {cursor}, {base}"));
        let here = self.calc(format!("icmp ult i64 {local}, {count}"));
        let n = self.next;
        self.next += 1;
        self.line(format!("br i1 {here}, label %clean{n}, label %skip{n}"));
        writeln!(self.code, "clean{n}:").unwrap();
        match field {
            FieldTy::Inline(t) => {
                let drop = self.recipes[t.index()].drop.as_ref().unwrap();
                let tag = self.fixed(TypeId(t.0)).tag;
                if let Some(tag) = tag {
                    let tagvalue = self.calc(format!("load i{}, ptr {p}, align 1", tag.width * 8));
                    self.line(format!(
                        "switch i{} {tagvalue}, label %row{n}_0 [",
                        tag.width * 8
                    ));
                    for row in 1..drop.fields.len() {
                        self.line(format!("i{} {row}, label %row{n}_{row}", tag.width * 8));
                    }
                    self.line("]");
                } else {
                    self.line(format!("br label %row{n}_0"));
                }
                for (row, fields) in drop.fields.iter().enumerate() {
                    writeln!(self.code, "row{n}_{row}:").unwrap();
                    let mut offset = 0;
                    for f in fields {
                        let at = self.calc(format!("getelementptr i8, ptr {p}, i64 {}", f.offset));
                        self.mechanical_field(&f.ty, &at, &local, offset)?;
                        offset += mechanical_count(self.module, self.recipes, &f.ty)?;
                    }
                    self.line("ret i1 false");
                }
            }
            FieldTy::Array(e, _) => {
                let per = mechanical_count(self.module, self.recipes, e)?;
                let index = self.calc(format!("udiv i64 {local}, {per}"));
                let inner = self.calc(format!("urem i64 {local}, {per}"));
                let stride = field_size(&self.module.types, self.recipes, e).0;
                let off = self.calc(format!("mul i64 {index}, {stride}"));
                let at = self.calc(format!("getelementptr i8, ptr {p}, i64 {off}"));
                self.mechanical_field(e, &at, &inner, 0)?;
                self.line("ret i1 false");
            }
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.calc(format!("load ptr, ptr {p}, align 1"));
                let logical = self.recipes[t.index()].logical_size.unwrap();
                let (name, charge) = if matches!(field, FieldTy::Box(_)) {
                    (
                        "hirsel_core_release_own_enqueue",
                        hirsel_core_layout::charge::boxed(logical).unwrap(),
                    )
                } else {
                    (
                        "hirsel_core_release_enqueue",
                        hirsel_core_layout::charge::counted(logical).unwrap(),
                    )
                };
                let mut args = vec![("ptr".into(), "%worklist".into()), ("ptr".into(), node)];
                if !matches!(field, FieldTy::Box(_)) {
                    args.push((
                        "i1 zeroext".into(),
                        matches!(field, FieldTy::Arc(_)).to_string(),
                    ));
                }
                args.extend([
                    ("i64".into(), charge.to_string()),
                    ("ptr".into(), format!("@core_counted_drop_{}", t.0)),
                ]);
                self.runtime_void(name, &args);
                self.line("ret i1 false");
            }
            _ => unreachable!(),
        }
        writeln!(self.code, "skip{n}:").unwrap();
        Ok(())
    }
}
fn counted_glue(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    demanded: &BTreeSet<usize>,
    text: &mut String,
    intrinsics: &mut BTreeSet<String>,
) -> Result<()> {
    let empty = Body::default();
    for &t in demanded {
        if module.types[t].repr == Repr::Resource {
            continue;
        }
        let mut e = FunctionEmitter {
            module,
            body: &empty,
            recipes,
            address_kinds: vec![],
            abi: abi::Return::new(&[]),
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics,
        };
        let field = FieldTy::Inline(TypeRef(t as u32));
        let cursor = e.calc("load i64, ptr %cursor");
        let next = e.calc(format!("add i64 {cursor}, 1"));
        e.line(format!("store i64 {next}, ptr %cursor"));
        e.mechanical_field(&field, "%payload", &cursor, 0)?;
        e.line("ret i1 true");
        writeln!(text,"define private zeroext i1 @core_counted_drop_{t}(ptr %context, ptr %payload, ptr %cursor, ptr %worklist) {{\nentry:\n{}{} }}",e.allocations,e.code).unwrap();
    }
    Ok(())
}
