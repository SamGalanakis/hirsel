//! Fixed runtime entries call ordinary CoreIR functions through their published
//! concrete convention. The input buffer also retains the plain code pointer.
use super::cleanup::{drop_signature, glue_emitter};
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn input_offset(
    module: &Module,
    recipes: &[hirsel_core_layout::RecipeRow],
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
pub(super) fn task_entry_name(x: TypeId, r: TypeId) -> String {
    format!("core_task_entry_{}_{}", x.0, r.0)
}
pub(super) fn transfer_entry_name(x: TypeId, r: TypeId) -> String {
    format!("core_transfer_entry_{}_{}", x.0, r.0)
}
/// The glue a transfer root resumes through once the runtime has freed its
/// moved input (`hirsel_core_transfer`): the code word is the input's only
/// word, and the job is read from a zero area that is never freed.
pub(super) fn transfer_resume_name(x: TypeId, r: TypeId) -> String {
    format!("core_transfer_resume_{}_{}", x.0, r.0)
}
fn transfer_zero_name(x: TypeId) -> String {
    format!("core_transfer_zero_{}", x.0)
}
/// Which entry a glue is: a spawned task's, a transfer root's, or a transfer
/// root's resume after its job was freed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Glue {
    Task,
    Transfer,
    Resume,
}
#[allow(clippy::too_many_arguments)]
pub(super) fn emit(
    module: &Module,
    members: Option<&[bool]>,
    target: Target,
    recipes: &[hirsel_core_layout::RecipeRow],
    call_layouts: &hirsel_core_ir::native_layout::CallLayouts,
    failures: &failure::Failures,
    facts: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    // A spawned task's code takes an environment first; a root a transfer
    // enters (`RtOp::Transfer`) is a function's own, with none.
    let pairs = super::bodies(module, members)
        .flat_map(|(_, b)| b.calls.iter())
        .filter_map(|c| match c.callee {
            Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r }) => {
                Some(vec![(x, r, Glue::Task)])
            }
            Callee::Runtime(RtOp::Transfer { x, r }) => {
                Some(vec![(x, r, Glue::Transfer), (x, r, Glue::Resume)])
            }
            _ => None,
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    // A resumed root takes its job from a zero area at least as large as the
    // job, so whatever the root's entry reads of it is in bounds; it reads
    // nothing it uses (`returnable::job_dead`).
    let zeros = pairs
        .iter()
        .filter(|&&(_, _, g)| g == Glue::Resume)
        .map(|&(x, _, _)| x)
        .collect::<BTreeSet<_>>();
    for x in zeros {
        let (bytes, _, _) = hirsel_core_layout::field_size(
            &module.types,
            recipes,
            &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(x.0)),
        );
        sink.global(&Global {
            name: transfer_zero_name(x).into(),
            linkage: Linkage::Private,
            constant: true,
            externally_initialized: false,
            ty: Type::bytes(bytes.max(8)),
            init: Some(Value::Zero),
            align: 8,
            absolute_symbol: false,
            section: None,
        });
    }
    let input = Value::Local(Local::INPUT);
    let body = Body::default();
    let result = Value::Local(Local::RESULT);
    for (x, r, glue) in pairs {
        let environment = glue == Glue::Task;
        let xc = module.types[x.index()].carried;
        let rc = module.types[r.index()].carried;
        let fields = match rc {
            Carried::Value(t) => vec![t],
            _ => vec![],
        };
        let argument = match xc {
            Carried::Address => call_layouts.record(x),
            _ => None,
        };
        let returned = match rc {
            Carried::Address => call_layouts.record(r),
            _ => None,
        };
        let abi = abi::Return::core_record(target, &fields, returned, true);
        sink.begin_function(
            &match glue {
                Glue::Task => task_entry_name(x, r),
                Glue::Transfer => transfer_entry_name(x, r),
                Glue::Resume => transfer_resume_name(x, r),
            },
            &FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr, Type::Ptr]),
            &[Local::CONTEXT, Local::RESULT, Local::INPUT],
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &body,
            abi::Return::on_target(target, &[Ty::Scalar(Scalar::U8)]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let code = e.load(&Type::Ptr, &input, 8);
        let data = match glue {
            Glue::Resume => Value::global(transfer_zero_name(x)),
            _ => e.offset(&input, input_offset(module, recipes, x)),
        };
        let mut args = vec![ptr(CONTEXT)];
        if matches!(rc, Carried::Address) && returned.is_none() {
            args.push(ptr(result.clone()));
        }
        if environment {
            args.push(ptr(Value::Null));
        }
        match xc {
            Carried::Address => {
                if let Some(record) = argument {
                    let values = e.load_record(record, &data);
                    args.extend(
                        record
                            .fields()
                            .iter()
                            .zip(values)
                            .map(|(&(_, t), v)| (abi.value_parameter(t), v)),
                    );
                } else {
                    args.push(ptr(data));
                }
            }
            Carried::Value(t) => {
                let v = e.load(&ty(t), &data, 1);
                args.push((
                    Parameter::extended(ty(t), abi::extension(e.abi.target, t)),
                    v,
                ));
            }
            Carried::Nothing => {}
        }
        args.push(word(int(0)));
        let values = e.invoke(&abi, &code, args, &CallAttrs::default());
        let status = values.last().unwrap().clone();
        if !fields.is_empty() || returned.is_some() {
            let success = e.cmp(Predicate::Eq, &Type::I8, &status, &int(0));
            let store = Label::plain("store_result");
            let done = Label::plain("done");
            e.cond(&success, store, done);
            e.block(store);
            if let Some(record) = returned {
                e.store_record(record, &result, &values);
            } else {
                e.store(&ty(fields[0]), &values[0], &result, 1);
            }
            e.br(done);
            e.block(done);
        }
        e.ret(&Type::I8, &status);
        drop(e);
        sink.end_function();
    }
    spawn_entries(module, target, recipes, failures, facts, sink, items)
}

/// Defines, weak, a marker per spawnable function's descriptor
/// ([`hirsel_core_ir::spawnable_symbol`]) and the spawn entry of each pair of
/// rows this unit spawns a
/// helper with ([`hirsel_core_ir::spawn_entry_symbol`]): `{bridge, adapter,
/// layout, x, r}` as the runtime's `SpawnEntry`. The link indexes them by the
/// identities of the functions such a helper can start, so a replay starts
/// one from its entry alone. The adapter is the spawn's own, which starts any
/// function of the rows' type.
fn spawn_entries(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::RecipeRow],
    failures: &failure::Failures,
    facts: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let mut units = BTreeMap::new();
    let mut spawnable = BTreeSet::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        // A function made a plain `fn(take X) -> R` value: the link joins its
        // descriptor to the spawn entry of its rows by this marker's name.
        for inst in &body.insts {
            if let Inst::DescriptorRef {
                function,
                spawn: Some(spawn),
                ..
            } = *inst
            {
                let name = module.descriptor_symbol(function, items.function_symbol(function));
                spawnable.insert(hirsel_core_ir::spawnable_symbol(
                    &name,
                    module.spawn_key(spawn),
                ));
            }
        }
        for call in &body.calls {
            if let Callee::Runtime(RtOp::SpawnUnit { x, r }) = call.callee
                && !units.contains_key(&(x, r))
            {
                units.insert((x, r), spawn_adapter(body, call)?);
            }
        }
    }
    for marker in spawnable {
        sink.global(&Global {
            name: marker.into(),
            linkage: Linkage::Weak,
            constant: true,
            externally_initialized: false,
            ty: Type::I8,
            init: Some(Value::Zero),
            align: 1,
            absolute_symbol: false,
            section: None,
        });
    }
    let body = Body::default();
    for ((x, r), adapter) in units {
        let e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &body,
            abi::Return::on_target(target, &[Ty::Scalar(Scalar::U8)]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let offset = input_offset(module, recipes, x);
        let layouts = [e.layout_words(x, true), e.layout_words(r, false)];
        let crosses = [e.cross_words(x, offset, true), e.cross_words(r, 0, false)];
        drop(e);
        let name = hirsel_core_ir::spawn_entry_symbol(module.spawn_key(SpawnTypes { x, r }));
        let words = |w: &[u64]| {
            w.iter()
                .map(|&n| (Type::I64, Value::Int(n as i128)))
                .collect::<Vec<_>>()
        };
        let mut fields = vec![
            (Type::Ptr, Value::global(task_entry_name(x, r))),
            (
                Type::Ptr,
                Value::global(items.function_symbol(adapter).to_owned()),
            ),
        ];
        let cleans = [Value::Null, Value::global(format!("core_clean_{}", r.0))];
        for (words_of, clean) in layouts.iter().zip(cleans) {
            fields.extend(words(words_of));
            fields.push((Type::Ptr, clean));
        }
        for (i, (cross, owners)) in crosses.iter().enumerate() {
            let table = if owners.is_empty() {
                Value::Null
            } else {
                let table = format!("{name}.owners.{i}");
                let bytes: Vec<u8> = owners
                    .iter()
                    .flatten()
                    .flat_map(|n| n.to_le_bytes())
                    .collect();
                sink.global(&Global {
                    name: table.clone().into(),
                    linkage: Linkage::Private,
                    constant: true,
                    externally_initialized: false,
                    ty: Type::bytes(bytes.len() as u64),
                    init: Some(Value::Bytes(bytes)),
                    align: 8,
                    absolute_symbol: false,
                    section: None,
                });
                Value::global(table)
            };
            fields.extend(words(&cross[..1]));
            fields.push((Type::Ptr, table));
            fields.extend(words(&cross[2..]));
        }
        sink.global(&Global {
            name: name.into(),
            linkage: Linkage::Weak,
            constant: true,
            externally_initialized: false,
            ty: Type::structure(fields.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()),
            init: Some(Value::Struct {
                packed: false,
                fields,
            }),
            align: 8,
            absolute_symbol: false,
            section: None,
        });
    }
    Ok(())
}

/// The function a spawn's input names as its adapter: `RtOp::SpawnUnit`'s
/// third operand, which the front makes a reference to the adapter it built.
fn spawn_adapter(body: &Body, call: &CallSite) -> Result<FuncId> {
    let args = call.args.get(&body.pools.args).unwrap();
    let Some(&Arg::Value(code)) = args.get(2) else {
        return Err(Error::Unsupported(
            "a unit spawn's adapter is not a value".into(),
        ));
    };
    body.insts
        .iter()
        .find_map(|inst| match *inst {
            Inst::FunctionRef { result, function } if result == code => Some(function),
            _ => None,
        })
        .ok_or_else(|| {
            Error::Unsupported("a unit spawn's adapter is not a function reference".into())
        })
}
