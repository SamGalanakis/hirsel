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
    ] {
        f.store(value, config, offset);
    }
    let names = f.address(data.names);
    let sources = f.address(data.sources);
    let entry = f.object.declare_func_in_func(entry, f.b.func);
    let entry = f.b.ins().func_addr(I64, entry);
    let status = f.value_call(
        "hirsel_core_os_run_with_sources",
        I8,
        &[config, names, sources, entry],
    )?;
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
