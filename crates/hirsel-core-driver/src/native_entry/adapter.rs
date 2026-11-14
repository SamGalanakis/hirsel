//! Decode the checked Result recipe, then clean its owned result exactly once.
use super::*;
use cranelift_codegen::ir::{InstBuilder, condcodes::IntCC};
use hirsel_cranelift::core::memory_layout as abi;

pub(super) fn emit(
    object: &mut ObjectModule,
    plan: &Plan<'_>,
    entry: FuncId,
    signature: ir::Signature,
) -> Result<()> {
    let mut context = object.make_context();
    context.func.signature = signature;
    let mut frontend = FunctionBuilderContext::new();
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut context.func, &mut frontend),
    };
    let block = f.b.create_block();
    f.b.append_block_params_for_function_params(block);
    f.b.switch_to_block(block);
    let params = f.b.block_params(block).to_vec();
    let size = u32::try_from(plan.layout.size.max(1))
        .map_err(|_| "entry result exceeds native stack-slot representation")?;
    let result = f.stack(size, plan.layout.align.trailing_zeros() as u8);
    let mut args = vec![params[0], result];
    if !plan.parameters.is_empty() {
        let grants = f.load(I64, params[1], abi::PROCESS_CONFIG_GRANTS);
        for i in 0..plan.parameters.len() {
            args.push(f.load(I64, grants, (i * 8) as i32));
        }
    }
    args.push(f.constant(0));
    let failed = f.value_call(&plan.symbol, I8, &args)?;
    let (failure, success) = f.if_nonzero(failed);
    f.b.switch_to_block(failure);
    f.b.ins().return_(&[failed]);
    f.b.switch_to_block(success);
    let tag = plan.layout.tag.expect("checked Result is a variant");
    let ty = match tag.width {
        1 => I8,
        2 => ir::types::I16,
        4 => I32,
        8 => I64,
        _ => unreachable!("fixed tag width"),
    };
    let word = f.load(
        ty,
        result,
        i32::try_from(tag.offset).map_err(|_| "entry tag offset exceeds native representation")?,
    );
    let word = if ty == I64 {
        word
    } else {
        f.b.ins().uextend(I64, word)
    };
    let case = hirsel_core_layout::read_case(&tag, &mut f, word);
    let is_error = f.b.ins().icmp_imm_u(IntCC::Equal, case, 1);
    let (error_case, cleaned) = f.if_nonzero(is_error);
    f.b.switch_to_block(error_case);
    let kind =
        f.b.ins()
            .iconst(I8, if plan.exit_offset.is_some() { 2 } else { 1 });
    f.store(kind, params[2], abi::MAIN_RESULT_KIND);
    if let Some(offset) = plan.exit_offset {
        let status = f.load(
            I8,
            result,
            i32::try_from(offset).map_err(|_| "Exit offset exceeds native representation")?,
        );
        f.store(status, params[2], abi::MAIN_RESULT_EXIT);
    }
    f.b.ins().jump(cleaned, &[]);
    f.b.switch_to_block(cleaned);
    let status = if let Some(cleanup) = &plan.cleanup {
        let site = f.constant(0);
        f.value_call(cleanup, I8, &[params[0], result, site])?
    } else {
        f.b.ins().iconst(I8, 0)
    };
    f.b.ins().return_(&[status]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    object.define_function(entry, &mut context).map_err(error)
}
