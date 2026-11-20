//! The C main owns argv views, calls the named OS boundary, then frees views.
use super::*;
use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};
use hirsel_cranelift::core::memory_layout as abi;

pub(super) fn emit(
    object: &mut ObjectModule,
    plan: &Plan<'_>,
    data: &data::Tables,
    entry: FuncId,
) -> Result<()> {
    let mut context = object.make_context();
    context.func.signature = signature(object, &[I32, I64], Some(I32));
    let main = object
        .declare_function("main", Linkage::Export, &context.func.signature)
        .map_err(error)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut context.func, &mut frontend),
    };
    let start = f.b.create_block();
    f.b.append_block_params_for_function_params(start);
    f.b.switch_to_block(start);
    let parameters = f.b.block_params(start).to_vec();
    let triple = f.object.isa().triple().to_string();
    if triple == hirsel_core_layout::Target::X86_64Linux.triple() {
        f.void_call("hirsel_core_init_isa", &[])?;
    }
    let argc = f.b.ins().uextend(I64, parameters[0]);
    let n = f.b.ins().iadd_imm_s(argc, -1);
    let one = f.constant(1);
    let size = f.constant(16);
    let count = f.b.ins().select(n, n, one);
    let args = f.value_call("calloc", I64, &[count, size])?;
    let (allocated, stopped) = f.if_nonzero(args);
    f.b.switch_to_block(stopped);
    let message = f.string(b"run stop: HostMemory\n")?;
    let stderr = f
        .object
        .declare_data("stderr", Linkage::Import, true, false)
        .map_err(error)?;
    let stderr_address = f.address(stderr);
    let stderr = f.load(I64, stderr_address, 0);
    f.value_call("fputs", I32, &[message, stderr])?;
    f.return_i32(102);
    f.b.switch_to_block(allocated);
    argv(&mut f, parameters[1], args, n)?;
    let config = f.stack(abi::OS_CONFIG_BYTES, 3);
    let directories = f.address(data.directories);
    let requirements = f.address(data.parameters);
    let limit = f.constant(plan.root_limit);
    let mask = f.constant(plan.grant_mask);
    let directories_len = f.constant(plan.directories.len() as u64);
    let parameters_len = f.constant(plan.parameters.len() as u64);
    let streams = f.constant((2u64 << 32) | 1);
    let net_rules = f.address(data.net_rules);
    let net_rules_len = f.constant(plan.net_rules.len() as u64);
    let target_data = data::bytes(f.object, None, triple.as_bytes().to_vec(), 1)?;
    let target = f.address(target_data);
    let target_len = f.constant(triple.len() as u64);
    let directory_ops = if plan.directories.is_empty() {
        f.constant(0)
    } else {
        let operations = f
            .object
            .declare_data(
                "hirsel_core_os_directory_ops",
                Linkage::Import,
                false,
                false,
            )
            .map_err(error)?;
        f.address(operations)
    };
    let process_kind = grant_kind("Process").unwrap();
    let process_ops = if plan.tape.is_none()
        && plan.grant_mask & (1 << process_kind) != 0
        && plan
            .parameters
            .iter()
            .any(|requirement| requirement.kind == process_kind)
    {
        let operations = f
            .object
            .declare_data("hirsel_core_os_process_ops", Linkage::Import, false, false)
            .map_err(error)?;
        f.address(operations)
    } else {
        f.constant(0)
    };
    for (offset, value) in [
        (abi::OS_CONFIG_ROOT_LIMIT, limit),
        (abi::OS_CONFIG_ARGS, args),
        (abi::OS_CONFIG_ARGS_LEN, n),
        (abi::OS_CONFIG_DIRECTORIES, directories),
        (abi::OS_CONFIG_DIRECTORIES_LEN, directories_len),
        (abi::OS_CONFIG_GRANT_MASK, mask),
        (abi::OS_CONFIG_STREAMS, streams),
        (abi::OS_CONFIG_PARAMETERS, requirements),
        (abi::OS_CONFIG_PARAMETERS_LEN, parameters_len),
        (abi::OS_CONFIG_NET_RULES, net_rules),
        (abi::OS_CONFIG_NET_RULES_LEN, net_rules_len),
        (abi::OS_CONFIG_TARGET, target),
        (abi::OS_CONFIG_TARGET_LEN, target_len),
        (abi::OS_CONFIG_DIRECTORY_OPS, directory_ops),
        (abi::OS_CONFIG_PROCESS_OPS, process_ops),
    ] {
        f.store(value, config, offset);
    }
    let names = f.address(data.names);
    let sources = f.address(data.sources);
    let entry = f.object.declare_func_in_func(entry, f.b.func);
    let entry = f.b.ins().func_addr(I64, entry);
    let status = if let Some(tape) = plan.tape {
        let path = f.string(tape.path.as_bytes())?;
        let identity = f.string(plan.identity.as_bytes())?;
        let mode = f.constant(tape.mode as u64);
        let revision = f
            .object
            .declare_data("hirsel_code_revision", Linkage::Import, false, false)
            .map_err(error)?;
        let revision = f.address(revision);
        f.value_call(
            "hirsel_core_os_run_taped",
            I8,
            &[
                config, names, sources, entry, path, mode, identity, revision,
            ],
        )?
    } else {
        let runner = if plan.launch_digest_unused {
            "hirsel_core_os_run_without_digest"
        } else {
            "hirsel_core_os_run_with_sources"
        };
        f.value_call(runner, I8, &[config, names, sources, entry])?
    };
    f.void_call("free", &[args])?;
    if let Some(path) = &plan.charge_report {
        report::emit(&mut f, path)?;
    }
    let status = f.b.ins().uextend(I32, status);
    f.b.ins().return_(&[status]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    object.define_function(main, &mut context).map_err(error)
}

fn argv(f: &mut Code<'_, '_>, argv: Value, args: Value, n: Value) -> Result<()> {
    let head = f.b.create_block();
    let body = f.b.create_block();
    let done = f.b.create_block();
    f.b.append_block_param(head, I64);
    let zero = f.constant(0);
    f.b.ins().jump(head, &[zero.into()]);
    f.b.switch_to_block(head);
    let i = f.b.block_params(head)[0];
    let more = f.b.ins().icmp(IntCC::UnsignedLessThan, i, n);
    f.b.ins().brif(more, body, &[], done, &[]);
    f.b.switch_to_block(body);
    let offset = f.b.ins().ishl_imm_u(i, 3);
    let address = f.b.ins().iadd(argv, offset);
    let argument = f.load(I64, address, 8);
    let len = f.value_call("strlen", I64, &[argument])?;
    let offset = f.b.ins().ishl_imm_u(i, 4);
    let view = f.b.ins().iadd(args, offset);
    f.store(argument, view, 0);
    f.store(len, view, 8);
    let next = f.b.ins().iadd_imm_u(i, 1);
    f.b.ins().jump(head, &[next.into()]);
    f.b.switch_to_block(done);
    Ok(())
}
