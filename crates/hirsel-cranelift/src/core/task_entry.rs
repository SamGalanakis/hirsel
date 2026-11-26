//! Native task-entry glue. The runtime owns a fixed C entry and an input buffer;
//! ordinary CoreIR callable signatures retain their concrete value convention.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Glues {
    pub entries: BTreeMap<(TypeId, TypeId), cranelift_module::FuncId>,
    /// A transfer's entry (`RtOp::Transfer`): a function's own root, which
    /// takes no environment.
    pub transfers: BTreeMap<(TypeId, TypeId), cranelift_module::FuncId>,
    /// By `RtOp::SpawnUnit` rows, the spawn entry `task.spawn_unit` passes
    /// ([`c::spawn_entry_symbol`]), defined by [`Self::define_units`].
    pub units: BTreeMap<(TypeId, TypeId), cranelift_module::DataId>,
}
impl Glues {
    pub fn declare(object: &mut ObjectModule, input: &c::Module) -> Result<Self> {
        let pairs = |transfer: bool| {
            input
                .functions
                .iter()
                .filter_map(|f| f.body.as_ref())
                .flat_map(|b| b.calls.iter())
                .filter_map(|c| match c.callee {
                    Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r })
                        if !transfer =>
                    {
                        Some((x, r))
                    }
                    Callee::Runtime(RtOp::Transfer { x, r }) if transfer => Some((x, r)),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        };
        let mut declare = |kind: &str, transfer: bool| {
            let mut glues = BTreeMap::new();
            for (x, r) in pairs(transfer) {
                let f = object
                    .declare_function(
                        &format!("hirsel.{kind}.entry.{}.{}", x.0, r.0),
                        Linkage::Local,
                        &abi::Signature::c(object, &[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif,
                    )
                    .map_err(error)?;
                glues.insert((x, r), f);
            }
            Ok::<_, Error>(glues)
        };
        Ok(Self {
            entries: declare("task", false)?,
            transfers: declare("transfer", true)?,
            units: BTreeMap::new(),
        })
    }

    /// Defines, weak, the spawn entry of each pair of rows this unit spawns a
    /// helper with: `{bridge, adapter, layout, x, r}` as the runtime's
    /// `SpawnEntry`. The link indexes them by the identities of the functions
    /// such a helper can start, so a replay starts one from its entry alone.
    /// The adapter is the spawn's own, which starts any function of the
    /// rows' type.
    pub fn define_units(
        &mut self,
        object: &mut ObjectModule,
        input: &c::Module,
        layouts: &[hirsel_core_layout::RecipeRow],
        functions: &[cranelift_module::FuncId],
        glues: &drop::Glues,
    ) -> Result<()> {
        for body in input.functions.iter().filter_map(|f| f.body.as_ref()) {
            for call in &body.calls {
                let Callee::Runtime(RtOp::SpawnUnit { x, r }) = call.callee else {
                    continue;
                };
                if self.units.contains_key(&(x, r)) {
                    continue;
                }
                let adapter = spawn_adapter(body, call)?;
                let name = c::spawn_entry_symbol(input.spawn_key(c::SpawnTypes { x, r }));
                let id = object
                    .declare_data(&name, Linkage::Preemptible, false, false)
                    .map_err(error)?;
                let offset = input_offset(input, layouts, x)?;
                let mut row = vec![0u8; 160];
                let mut put = |at: usize, n: u64| row[at..at + 8].copy_from_slice(&n.to_le_bytes());
                for (i, (t, task_input)) in [(x, true), (r, false)].into_iter().enumerate() {
                    let words = layout_words(input, layouts, t, task_input)?;
                    for (j, n) in words.into_iter().enumerate() {
                        put(16 + i * 32 + j * 8, n);
                    }
                }
                let mut owners = Vec::new();
                for (i, (t, shift, entry)) in
                    [(x, offset, true), (r, 0, false)].into_iter().enumerate()
                {
                    let (words, rows) = cross_words(input, layouts, t, shift, entry)?;
                    for (j, n) in words.into_iter().enumerate() {
                        put(80 + i * 40 + j * 8, n);
                    }
                    owners.push(rows);
                }
                let mut data = DataDescription::new();
                data.define(row.into_boxed_slice());
                data.set_align(8);
                let bridge = object.declare_func_in_data(self.entries[&(x, r)], &mut data);
                data.write_function_addr(0, bridge);
                let adapter = object.declare_func_in_data(functions[adapter.index()], &mut data);
                data.write_function_addr(8, adapter);
                let clean = glues.values[r.index()].expect("a spawned result's cleanup");
                let clean = object.declare_func_in_data(clean, &mut data);
                data.write_function_addr(48 + 24, clean);
                for (i, rows) in owners.into_iter().enumerate() {
                    if rows.is_empty() {
                        continue;
                    }
                    let table = object.declare_anonymous_data(false, false).map_err(error)?;
                    let mut words = DataDescription::new();
                    words.define(
                        rows.iter()
                            .flatten()
                            .flat_map(|n| n.to_le_bytes())
                            .collect(),
                    );
                    words.set_align(8);
                    object.define_data(table, &words).map_err(error)?;
                    let table = object.declare_data_in_data(table, &mut data);
                    data.write_data_addr((80 + i * 40 + 8) as u32, table, 0);
                }
                object.define_data(id, &data).map_err(error)?;
                self.units.insert((x, r), id);
            }
        }
        Ok(())
    }
    pub fn emit(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        let glues = self
            .entries
            .iter()
            .map(|(&k, &f)| (k, f, true))
            .chain(self.transfers.iter().map(|(&k, &f)| (k, f, false)));
        for ((x, r), f, environment) in glues {
            let context = &mut scratch.codegen;
            object.clear_context(context);
            context.func.signature =
                abi::Signature::c(object, &[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif;
            let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let ps = b.block_params(entry).to_vec();
            let code = b.ins().load(types::I64, MemFlagsData::trusted(), ps[2], 0);
            let data = b
                .ins()
                .iadd_imm(ps[2], input_offset(tables.input, tables.layouts, x)? as i64);
            let xc = tables.input.types[x.index()].carried;
            let rc = tables.input.types[r.index()].carried;
            let params = match xc {
                // The task takes its input (RtOp::Spawn).
                Carried::Address => vec![Param::Address {
                    place: c::PlaceTy::Declared(x),
                    loan: c::Loan::Take,
                }],
                Carried::Value(t) => vec![Param::Value(t)],
                Carried::Nothing => vec![],
            };
            let results = match rc {
                Carried::Value(t) => vec![t],
                _ => vec![],
            };
            let dest = matches!(rc, Carried::Address).then_some(r);
            let mut sig = c::Signature::new(params, results, dest);
            // A spawned task's code takes an environment first; a root a
            // transfer enters is a function's own, with none.
            if environment {
                sig.params.insert(0, Param::Value(Ty::Ptr));
            }
            let job = usize::from(environment);
            let abi = abi::Signature::new(&sig, object, tables.call_layouts);
            let signature = b.import_signature(abi.clif.clone());
            let mut args = Vec::with_capacity(4);
            args.push(ps[0]);
            if abi.dest {
                args.push(ps[1]);
            }
            if environment {
                args.push(b.ins().iconst(types::I64, 0));
            }
            match xc {
                Carried::Address => {
                    if let Some(record) = abi.param_records[job] {
                        abi::load_record(&mut b, record, data, &mut args);
                    } else {
                        args.push(data);
                    }
                }
                Carried::Value(t) => {
                    let v = b.ins().load(ty(t), MemFlagsData::trusted(), data, 0);
                    args.push(v)
                }
                Carried::Nothing => {}
            }
            args.push(b.ins().iconst(types::I64, 0));
            let call = b.ins().call_indirect(signature, code, &args);
            let physical = b.inst_results(call).to_vec();
            let mut values = [Value::from_u32(0); 3];
            let mut n = 0;
            abi.unpack(&mut b, &physical, None, |v| {
                values[n] = v;
                n += 1
            });
            let status = values[n - 1];
            if matches!(rc, Carried::Value(_)) || abi.record.is_some() {
                let success = b.create_block();
                let done = b.create_block();
                let ok = b.ins().icmp_imm(IntCC::Equal, status, 0);
                b.ins().brif(ok, success, &[], done, &[]);
                b.switch_to_block(success);
                if let Some(record) = abi.record {
                    abi::store_record(&mut b, record, ps[1], &values[..n - 1]);
                } else {
                    b.ins().store(MemFlagsData::trusted(), values[0], ps[1], 0);
                }
                b.ins().jump(done, &[]);
                b.switch_to_block(done);
            }
            b.ins().return_(&[status]);
            b.seal_all_blocks();
            b.finalize(object.target_config());
            tables.define(observer, object, f, context)?;
        }
        Ok(())
    }
}
pub(super) fn input_offset(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    x: TypeId,
) -> Result<u64> {
    let (_, align) = place::storage(input, layouts, PlaceTy::Declared(x))?;
    Ok(align.max(8))
}

/// The function a spawn's input names as its adapter: `RtOp::SpawnUnit`'s
/// third operand, which the front makes a reference to the adapter it built.
fn spawn_adapter(body: &c::Body, call: &CallSite) -> Result<c::FuncId> {
    let args = call.args.get(&body.pools.args).unwrap();
    let Some(&Arg::Value(code)) = args.get(2) else {
        return Err(unsupported("a unit spawn's adapter is not a value"));
    };
    body.insts
        .iter()
        .find_map(|inst| match *inst {
            c::Inst::FunctionRef { result, function } if result == code => Some(function),
            _ => None,
        })
        .ok_or_else(|| unsupported("a unit spawn's adapter is not a function reference"))
}

/// A `ValueLayout`'s words before its cleanup: bytes, alignment and logical
/// size. A task input's bytes and alignment cover its entry's code word.
pub(super) fn layout_words(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    t: TypeId,
    task_input: bool,
) -> Result<[u64; 3]> {
    let (mut bytes, mut alignment) = place::storage(input, layouts, PlaceTy::Declared(t))?;
    if task_input {
        bytes += input_offset(input, layouts, t)?;
        alignment = alignment.max(8);
    }
    let logical = hirsel_core_layout::field_size(
        &input.types,
        layouts,
        &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
    )
    .2;
    Ok([bytes, alignment, logical])
}

/// A `Cross` of a value of `ty` at `shift` bytes into a buffer of `shift +
/// size(ty)`: its words, the second (the owner rows' address) zero, and its
/// owner rows. It spans the value a recorder writes (a task entry's `x`
/// when `entry`).
pub(super) fn cross_words(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    ty: TypeId,
    shift: u64,
    entry: bool,
) -> Result<([u64; 5], Vec<[u64; 4]>)> {
    let (bytes, _) = place::storage(input, layouts, PlaceTy::Declared(ty))?;
    let row = hirsel_core_layout::TypeRef(ty.0);
    let mut owners = hirsel_core_layout::cross::owners(&input.types, layouts, row);
    let [at, len] = if entry {
        hirsel_core_layout::cross::entry_span(&input.types, layouts, row)
    } else {
        [0, bytes]
    };
    for row in &mut owners {
        // An element row's offset is in its element.
        if row[1] & hirsel_core_layout::cross::ELEMENT == 0 {
            row[0] += shift;
        }
    }
    Ok((
        [bytes + shift, 0, owners.len() as u64, at + shift, len],
        owners,
    ))
}
