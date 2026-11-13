//! Recipe cleanup and resumable counted destruction.
use super::*;
use hirsel_core_layout::{FieldTy, Repr, TypeRef, field_size};

fn cleanup_dependencies(field: &FieldTy, counted: bool, pending: &mut Vec<(usize, bool)>) {
    match field {
        FieldTy::Inline(t) | FieldTy::Box(t) => pending.push((t.index(), counted)),
        FieldTy::Rc(t) | FieldTy::Arc(t) => pending.push((t.index(), true)),
        FieldTy::Map(key, value) => {
            cleanup_dependencies(key, counted, pending);
            cleanup_dependencies(value, counted, pending);
        }
        FieldTy::Array(element, _) | FieldTy::Seq(element) => {
            cleanup_dependencies(element, counted, pending)
        }
        _ => {}
    }
}

pub(super) fn drop_glue(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
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
                Inst::Call { call } => match body.calls[call.index()].callee {
                    Callee::Runtime(RtOp::Spawn { x, r }) => {
                        pending.push((x.index(), false));
                        pending.push((r.index(), false));
                    }
                    Callee::Runtime(RtOp::ChannelNew { item }) => {
                        pending.push((item.index(), false))
                    }
                    _ => {}
                },
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
            // FORM v1c: counted cleanup runs a row with a finalizer whole, by its
            // ordinary glue.
            if mechanical && drop.finalizer.is_some() {
                pending.push((t, false));
                continue;
            }
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
            abi: abi::Return::internal(&[]),
            has_status: true,
            failures,
            constants: &[],
            observe_charge: false,
            facts,
            aliases: None,
            empty_bounds: empty::EmptyBounds::default(),
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
        #[cfg(test)]
        if resource
            && let hirsel_core_layout::Body::Record(fields) = &module.types[t].body
            && let Some(i) = fields
                .iter()
                .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
        {
            let offset = e.fixed(TypeId(t as u32)).offsets[0][i];
            let field = e.calc(format!("getelementptr i8, ptr {base}, i64 {offset}"));
            let id = e.calc(format!("load i64, ptr {field}, align 1"));
            e.runtime_void("hirsel_core_test_drop_trace", &[("i64".into(), id)]);
        }
        // A resource's finalizer runs under its grant; a core slot kernel's on an
        // own row runs under the current limit (FORM v1c).
        if let Some(f) = module.types[t].finalizer {
            if resource {
                e.runtime_void(
                    "hirsel_core_finalizer_begin",
                    &[
                        ("ptr".into(), "%context".into()),
                        ("ptr".into(), base.clone()),
                    ],
                );
            }
            if failures.may_fail[f.0 as usize] {
                let failed = e.calc(format!(
                    "call i8 @{}(ptr %context, ptr {base})",
                    symbol(module, FuncId(f.0))
                ));
                e.line(format!("store i8 {failed}, ptr {status}"));
            } else {
                e.line(format!(
                    "call void @{}.infallible(ptr %context, ptr {base})",
                    symbol(module, FuncId(f.0))
                ));
            }
            if resource {
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
                let which = e.case_value(&tag, &base);
                e.line(format!("switch i32 {which}, label %case0 ["));
                for row in 1..drop.fields.len() {
                    e.line(format!("i32 {row}, label %case{row}"));
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
    let mut sequences = Vec::new();
    for &t in &counted {
        if let Some(drop) = &recipes[t].drop {
            for field in drop.fields.iter().flatten() {
                collections::collect(&field.ty, &mut sequences);
            }
        }
    }
    counted_glue(module, recipes, failures, facts, &counted, text, intrinsics)?;
    collections::glue(
        module, recipes, failures, facts, &sequences, text, intrinsics,
    )?;
    Ok(())
}

impl FunctionEmitter<'_> {
    pub(super) fn clean_field(&mut self, field: &FieldTy, p: &str, status: &str) -> Result<()> {
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
                self.free_block(&block, &charge.to_string());
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
            FieldTy::Seq(_) | FieldTy::Map(_, _) => {
                self.clean_collection(field, p, status)?;
                None
            }
            FieldTy::OwnFn => {
                let at = self.calc(format!("getelementptr i8, ptr {p}, i64 8"));
                let environment = self.calc(format!("load ptr, ptr {at}, align 1"));
                let allocated = self.calc(format!("icmp ne ptr {environment}, null"));
                let label = self.next;
                self.next += 1;
                self.line(format!(
                    "br i1 {allocated}, label %closure_drop{label}, label %closure_done{label}"
                ));
                writeln!(self.code, "closure_drop{label}:").unwrap();
                let glue = self.calc(format!("load ptr, ptr {environment}, align 1"));
                self.line(format!("call i8 {glue}(ptr %context, ptr {environment})"));
                self.line(format!("br label %closure_done{label}"));
                writeln!(self.code, "closure_done{label}:").unwrap();
                None
            }
            FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.calc(format!("load ptr, ptr {p}, align 1"));
                self.release_node(&node, *t, matches!(field, FieldTy::Arc(_)))?;
                None
            }
            FieldTy::Scalar(_)
            | FieldTy::Stable(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_) => None,
            FieldTy::Handle(hirsel_core_layout::Handle::HostFile) => {
                let handle = self.calc(format!("load i64, ptr {p}, align 1"));
                self.runtime_void(
                    "hirsel_core_hostfile_drop",
                    &[("ptr".into(), "%context".into()), ("i64".into(), handle)],
                );
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Causes) => {
                let chain = self.calc(format!("load ptr, ptr {p}, align 1"));
                self.runtime_void("hirsel_core_causes_drop", &[ptr(&chain)]);
                None
            }
            FieldTy::Handle(
                hirsel_core_layout::Handle::Task | hirsel_core_layout::Handle::Receiver,
            ) => {
                let handle = self.calc(format!("load ptr, ptr {p}, align 1"));
                let name = if *field == FieldTy::Handle(hirsel_core_layout::Handle::Task) {
                    "hirsel_core_task_drop"
                } else {
                    "hirsel_core_receiver_drop"
                };
                Some(
                    self.native_call(
                        name,
                        &[Ty::Scalar(Scalar::U8)],
                        &[ptr("%context"), ptr(&handle)],
                    )
                    .remove(0),
                )
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Sender) => {
                let handle = self.calc(format!("load ptr, ptr {p}, align 1"));
                self.runtime_void("hirsel_core_sender_drop", &[ptr(&handle)]);
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Capability) => None,
            other => return Err(Error::Unsupported(format!("drop field {other:?}"))),
        };
        if let Some(failed) = failed {
            let old = self.calc(format!("load i8, ptr {status}"));
            let both = self.calc(format!("or i8 {old}, {failed}"));
            self.line(format!("store i8 {both}, ptr {status}"));
        }
        Ok(())
    }
    pub(super) fn release_node(&mut self, node: &str, t: TypeRef, atomic: bool) -> Result<()> {
        let logical = self.recipes[t.index()].logical_size.unwrap();
        let charge = hirsel_core_layout::charge::counted(logical).unwrap();
        self.release_fast(node, charge, &format!("@core_counted_drop_{}", t.0), atomic);
        Ok(())
    }
}

pub(super) fn mechanical_count(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    field: &FieldTy,
) -> Result<u64> {
    Ok(match field {
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Seq(_)
        | FieldTy::Map(_, _) => 1,
        FieldTy::Inline(t) => {
            let Some(drop) = &recipes[t.index()].drop else {
                return Ok(0);
            };
            // FORM v1c: a row with a finalizer is one step, cleaned whole.
            if drop.finalizer.is_some() {
                return Ok(1);
            }
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
    pub(super) fn mechanical_field(
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
            // Its ordinary glue: the finalizer, then the fields. A counted payload
            // holds no resource, so this cannot fail.
            FieldTy::Inline(t)
                if self.recipes[t.index()]
                    .drop
                    .as_ref()
                    .is_some_and(|d| d.finalizer.is_some()) =>
            {
                self.line(format!("call i8 @core_drop_{}(ptr %context, ptr {p})", t.0));
                self.line("ret i1 false");
            }
            FieldTy::Inline(t) => {
                let drop = self.recipes[t.index()].drop.as_ref().unwrap();
                let tag = self.fixed(TypeId(t.0)).tag;
                if let Some(tag) = tag {
                    let tagvalue = self.case_value(&tag, &p);
                    self.line(format!("switch i32 {tagvalue}, label %row{n}_0 ["));
                    for row in 1..drop.fields.len() {
                        self.line(format!("i32 {row}, label %row{n}_{row}"));
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
            FieldTy::Seq(_) | FieldTy::Map(_, _) => self.enqueue_collection(field, p),
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
    failures: &failure::Failures,
    facts: bool,
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
            abi: abi::Return::new(&[]),
            has_status: false,
            failures,
            constants: &[],
            observe_charge: false,
            facts,
            aliases: None,
            empty_bounds: empty::EmptyBounds::default(),
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
