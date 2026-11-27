//! The embedded entry adapter and its immutable launch tables.
use super::*;
use cranelift_codegen::ir::InstBuilder;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit(
    object: &mut ObjectModule,
    unwind: &mut Unwind,
    target: Target,
    plan: &Plan<'_>,
    tables: &data::Tables,
    entry: FuncId,
    revision: DataId,
    manifest: &[u8],
    waits: bool,
) -> Result<()> {
    let manifest_len = manifest.len() as u64;
    let manifest = data::bytes(object, None, manifest.to_vec(), 1)?;
    let triple = target.triple();
    let target_data = data::bytes(object, None, triple.as_bytes().to_vec(), 1)?;

    let mut start = object.make_context();
    start.func.signature = signature(object, &[I64, I64], Some(I32));
    let start_id = object
        .declare_function(
            "hirsel_archive_start",
            Linkage::Export,
            &start.func.signature,
        )
        .map_err(error)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut start.func, &mut frontend),
    };
    let block = f.b.create_block();
    f.b.append_block_params_for_function_params(block);
    f.b.switch_to_block(block);
    let config = f.b.block_params(block)[0];
    let out = f.b.block_params(block)[1];
    let entry = f.object.declare_func_in_func(entry, f.b.func);
    let args = [
        config,
        out,
        f.address(manifest),
        f.constant(manifest_len),
        f.address(target_data),
        f.constant(triple.len() as u64),
        f.constant(plan.root_limit),
        f.address(tables.parameters),
        f.constant(plan.parameters.len() as u64),
        f.address(tables.names),
        f.address(revision),
        f.constant(u64::from(plan.place_backend.is_some())),
        f.constant(u64::from(waits)),
        f.address(tables.sources),
        f.b.ins().func_addr(I64, entry),
    ];
    let status = f.value_call("hirsel_embed_archive_start", I32, &args)?;
    f.b.ins().return_(&[status]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    unwind.define(object, start_id, &mut start).map_err(error)?;

    let mut get = object.make_context();
    get.func.signature = signature(object, &[], Some(I64));
    get.func.signature.returns.push(ir::AbiParam::new(I64));
    let get_id = object
        .declare_function(
            "hirsel_archive_manifest",
            Linkage::Export,
            &get.func.signature,
        )
        .map_err(error)?;
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut get.func, &mut frontend),
    };
    let block = f.b.create_block();
    f.b.switch_to_block(block);
    let ptr = f.address(manifest);
    let len = f.constant(manifest_len);
    f.b.ins().return_(&[ptr, len]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    unwind.define(object, get_id, &mut get).map_err(error)
}
