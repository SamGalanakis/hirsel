//! Fixed runtime entries call ordinary CoreIR functions through their published
//! concrete convention. The input buffer also retains the plain code pointer.
use super::*;

pub(super) fn input_offset(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    x: TypeId,
) -> u64 {
    hirsel_core_layout::field_size(
        &module.types,
        recipes,
        &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(x.0)),
    )
    .1
    .max(8)
}
pub(super) fn emit(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    text: &mut String,
    intrinsics: &mut BTreeSet<String>,
) -> Result<()> {
    let pairs = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|b| b.calls.iter())
        .filter_map(|c| match c.callee {
            Callee::Runtime(RtOp::Spawn { x, r }) => Some((x, r)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let inputs = pairs.iter().map(|(x, _)| *x).collect::<BTreeSet<_>>();
    for x in inputs {
        let offset = input_offset(module, recipes, x);
        writeln!(text,"define private i8 @core_task_input_drop_{}(ptr %context, ptr %input) {{\nentry:\n  %x = getelementptr i8, ptr %input, i64 {offset}\n  %status = call i8 @core_drop_{}(ptr %context, ptr %x)\n  ret i8 %status\n}}",x.0,x.0).unwrap();
    }
    let body = Body::default();
    for (x, r) in pairs {
        let xc = module.types[x.index()].carried;
        let rc = module.types[r.index()].carried;
        let fields = match rc {
            Carried::Value(t) => vec![t],
            _ => vec![],
        };
        let abi = abi::Return::internal(&fields);
        let mut e = FunctionEmitter {
            module,
            body: &body,
            recipes,
            abi: abi::Return::new(&[Ty::Scalar(Scalar::U8)]),
            has_status: false,
            failures,
            constants: &[],
            empty_bounds: empty::EmptyBounds::new(&body, &[]),
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics,
        };
        let code = e.calc("load ptr, ptr %input, align 8");
        let data = e.calc(format!(
            "getelementptr i8, ptr %input, i64 {}",
            input_offset(module, recipes, x)
        ));
        let mut args = vec!["ptr %context".into()];
        if matches!(rc, Carried::Address) {
            args.push("ptr %result".into())
        }
        match xc {
            Carried::Address => args.push(format!("ptr {data}")),
            Carried::Value(t) => {
                let v = e.calc(format!("load {}, ptr {data}, align 1", ty(t)));
                args.push(format!("{}{} {v}", ty(t), abi::extension(t)))
            }
            Carried::Nothing => {}
        }
        let values = e.invoke(&abi, &code, args);
        let status = values.last().unwrap();
        if let Carried::Value(t) = rc {
            let success = e.calc(format!("icmp eq i8 {status}, 0"));
            e.line(format!("br i1 {success}, label %store_result, label %done"));
            e.line("store_result:");
            e.line(format!(
                "store {} {}, ptr %result, align 1",
                ty(t),
                values[0]
            ));
            e.line("br label %done");
            e.line("done:");
        }
        e.line(format!("ret i8 {status}"));
        writeln!(text,"define private i8 @core_task_entry_{}_{}(ptr %context, ptr %result, ptr %input) {{\nentry:\n{}{}\n}}",x.0,r.0,e.allocations,e.code).unwrap();
    }
    Ok(())
}
