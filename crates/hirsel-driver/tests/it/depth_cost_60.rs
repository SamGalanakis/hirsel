//! MEM-3 checks disappear only when the independently validated type bounds them.
use hirsel_engine::{QueryEngine, default_context, default_environment, source_check_request};
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::TargetSpec;
use hirsel_native::emit_ir;

#[test]
fn bounded_structural_types_need_no_native_preflight() {
    with_compiler_stack(|| {
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("bounded.hir", "pub fn main() -> bool {\n  let a = [1u64, 2u64]\n  let b = [1u64, 2u64]\n  a == b\n}\n".to_owned()).unwrap();
        let output = source_check_request(&mut engine, file, &default_context()).unwrap();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let input = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(input).unwrap();
        for target in TargetSpec::executed() {
            let ready = {
 let options = &Options::new(target);
 hirsel_requests::source_ready_supplier(&"pub fn main() -> bool {\n  let a = [1u64, 2u64]\n  let b = [1u64, 2u64]\n  a == b\n}\n", "bounded.hir", &hirsel_engine::entry_path(&hirsel_engine::default_context()), &hirsel_native_testing::ready_toolchain(&options.target)).unwrap()(options)
}.unwrap();
            let ready = &ready.program;
            let ir = emit_ir(&ready, OptLevel::None).unwrap();
            assert!(!ir.contains("@hirsel_rt_v2_depth_preflight("), "{ir}");
        }
    });
}
