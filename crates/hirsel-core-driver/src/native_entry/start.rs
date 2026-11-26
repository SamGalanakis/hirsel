//! The C main owns argv views, calls the named OS boundary, then frees views.
use super::*;
use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};
use hirsel_cranelift::core::memory_layout as abi;

pub(super) fn emit(
    object: &mut ObjectModule,
    unwind: &mut Unwind,
    plan: &Plan<'_>,
    data: &data::Tables,
    entry: FuncId,
    foreign_waits: bool,
    spawns_units: bool,
    build: Option<DataId>,
    descriptors: Option<(DataId, DataId)>,
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
    let target = hirsel_core_layout::Target::from_triple(&triple)
        .ok_or_else(|| format!("no Hirsel target {triple}"))?;
    match target.arch() {
        hirsel_core_layout::Arch::X86_64 => f.void_call("hirsel_core_init_isa", &[])?,
        hirsel_core_layout::Arch::Aarch64 => {}
    }
    if plan.coverage {
        coverage_open(&mut f)?;
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
        .declare_data(
            // libSystem names its stderr stream `__stderrp`.
            match target.os() {
                hirsel_core_layout::Os::Linux => "stderr",
                hirsel_core_layout::Os::Macos => "__stderrp",
            },
            Linkage::Import,
            true,
            false,
        )
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
    let net_rules_len = f.constant((plan.net_rules.len() + plan.unix_roots.len()) as u64);
    let target_data = data::bytes(f.object, None, triple.as_bytes().to_vec(), 1)?;
    let target_address = f.address(target_data);
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
    let process_ops = if plan.replay.is_none()
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
    let net_kind = grant_kind("Net").unwrap();
    let socket_activation = if plan.replay.is_none() && plan.grant_mask & (1 << net_kind) != 0 {
        let activation = f
            .object
            .declare_data(
                "hirsel_core_os_socket_activation",
                Linkage::Import,
                false,
                false,
            )
            .map_err(error)?;
        f.address(activation)
    } else {
        f.constant(0)
    };
    let secret_ops = if plan.replay.is_none() && !plan.secret_grants.is_empty() {
        let operations = f
            .object
            .declare_data("hirsel_core_os_secret_ops", Linkage::Import, false, false)
            .map_err(error)?;
        f.address(operations)
    } else {
        f.constant(0)
    };
    let env_ops =
        if plan.replay.is_none() && plan.grant_mask & (1 << grant_kind("Env").unwrap()) != 0 {
            let operations = f
                .object
                .declare_data("hirsel_core_os_env_ops", Linkage::Import, false, false)
                .map_err(error)?;
            f.address(operations)
        } else {
            f.constant(0)
        };
    let exec_ops = if plan.exec.is_empty() {
        f.constant(0)
    } else {
        let operations = f
            .object
            .declare_data("hirsel_core_os_exec_ops", Linkage::Import, false, false)
            .map_err(error)?;
        f.address(operations)
    };
    let revision = f
        .object
        .declare_data("hirsel_code_revision", Linkage::Import, false, false)
        .map_err(error)?;
    let revision = f.address(revision);
    let launch_word = launch_config::emit(f.object, target.object_format(), plan.place)?;
    let launch_word = f.address(launch_word);
    let place = f.load(I64, launch_word, 0);
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
        (abi::OS_CONFIG_TARGET, target_address),
        (abi::OS_CONFIG_TARGET_LEN, target_len),
        (abi::OS_CONFIG_DIRECTORY_OPS, directory_ops),
        (abi::OS_CONFIG_PROCESS_OPS, process_ops),
        (abi::OS_CONFIG_SOCKET_ACTIVATION, socket_activation),
        (abi::OS_CONFIG_SECRET_GRANTS, f.address(data.secret_grants)),
        (
            abi::OS_CONFIG_SECRET_GRANTS_LEN,
            f.constant(plan.secret_grants.len() as u64),
        ),
        (abi::OS_CONFIG_SECRET_OPS, secret_ops),
        (abi::OS_CONFIG_CRED_GRANTS, f.address(data.cred_grants)),
        (
            abi::OS_CONFIG_CRED_GRANTS_LEN,
            f.constant(plan.cred_grants.len() as u64),
        ),
        (abi::OS_CONFIG_ENV_PATTERNS, f.address(data.env_patterns)),
        (
            abi::OS_CONFIG_ENV_PATTERNS_LEN,
            f.constant(plan.env_patterns.as_ref().map_or(0, String::len) as u64),
        ),
        (abi::OS_CONFIG_ENV_OPS, env_ops),
        (abi::OS_CONFIG_EXEC_BINDINGS, f.address(data.exec_bindings)),
        (
            abi::OS_CONFIG_EXEC_BINDINGS_LEN,
            f.constant(plan.exec.len() as u64),
        ),
        (abi::OS_CONFIG_EXEC_OPS, exec_ops),
        (abi::OS_CONFIG_REVISION, revision),
        (abi::OS_CONFIG_PLACE, place),
    ] {
        f.store(value, config, offset);
    }
    let names = f.address(data.names);
    let sources = f.address(data.sources);
    let entry = f.object.declare_func_in_func(entry, f.b.func);
    let entry = f.b.ins().func_addr(I64, entry);
    let build = build.map(|build| f.address(build));
    let status = if let (Some(replay), Some(build)) = (plan.replay, build) {
        let path = f.string(replay.path.as_bytes())?;
        let identity = f.string(plan.identity.as_bytes())?;
        let mode = f.constant(if replay.diagnose { 2 } else { 1 });
        let (descriptors, spawns) = match descriptors {
            Some((descriptors, spawns)) => (f.address(descriptors), f.address(spawns)),
            None => (f.constant(0), f.constant(0)),
        };
        f.value_call(
            "hirsel_core_os_replay",
            I8,
            &[
                config,
                names,
                sources,
                entry,
                path,
                mode,
                identity,
                revision,
                build,
                descriptors,
                spawns,
            ],
        )?
    } else if let Some(build) = build {
        let identity = f.string(plan.identity.as_bytes())?;
        match descriptors {
            Some((descriptors, spawns)) => {
                let descriptors = f.address(descriptors);
                let spawns = f.address(spawns);
                f.value_call(
                    "hirsel_core_os_run_recording_with_units",
                    I8,
                    &[
                        config,
                        names,
                        sources,
                        entry,
                        identity,
                        revision,
                        build,
                        descriptors,
                        spawns,
                    ],
                )?
            }
            None => f.value_call(
                "hirsel_core_os_run_recording",
                I8,
                &[config, names, sources, entry, identity, revision, build],
            )?,
        }
    } else if !plan.foreign.is_empty() {
        let waits = f.b.ins().iconst(I8, i64::from(foreign_waits));
        let runner = if spawns_units {
            "hirsel_core_os_run_foreign_with_units"
        } else {
            "hirsel_core_os_run_foreign_with_sources"
        };
        f.value_call(runner, I8, &[config, names, sources, entry, waits])?
    } else if spawns_units {
        let omit_digest = f.b.ins().iconst(I8, i64::from(plan.launch_digest_unused));
        f.value_call(
            "hirsel_core_os_run_with_units",
            I8,
            &[config, names, sources, entry, omit_digest],
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
    if plan.coverage {
        // The test's result decides the exit status here, so the outcome is
        // written here: a run that never returns leaves it "not recorded".
        f.void_call("hirsel_core_coverage_outcome", &[status])?;
    }
    f.b.ins().return_(&[status]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    unwind.define(object, main, &mut context).map_err(error)
}

/// Map the flag section onto the raw file before user code and before any
/// thread starts. The entry's own eight zero bytes are a record-free padding
/// word, so the section and its linker-defined bounds exist in every coverage
/// executable, also one whose bodies have no sites.
fn coverage_open(f: &mut Code<'_, '_>) -> Result<()> {
    let mut padding = DataDescription::new();
    padding.define(vec![0; 8].into_boxed_slice());
    padding.set_align(8);
    padding.set_custom_section(hirsel_core_ir::coverage::SECTION);
    padding.set_used(true);
    let id = f
        .object
        .declare_anonymous_data(true, false)
        .map_err(error)?;
    f.object.define_data(id, &padding).map_err(error)?;
    let mut bound = |name: String| -> Result<Value> {
        let id = f
            .object
            .declare_data(&name, Linkage::Import, true, false)
            .map_err(error)?;
        Ok(f.address(id))
    };
    let section = hirsel_core_ir::coverage::SECTION;
    let start = bound(format!("__start_{section}"))?;
    let stop = bound(format!("__stop_{section}"))?;
    let length = f.b.ins().isub(stop, start);
    f.void_call("hirsel_core_coverage_open", &[start, length])
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
