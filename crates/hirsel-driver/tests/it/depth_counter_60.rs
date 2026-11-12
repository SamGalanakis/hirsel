//! Validated source admission scopes lower to local counter updates on both targets.
use hirsel_engine::{QueryEngine, default_context, default_environment, source_check_request};
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::TargetSpec;
use hirsel_native::emit_ir;

#[test]
fn recursive_scalar_calls_use_counters_without_invocation_metadata() {
    with_compiler_stack(|| {
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("counter.hir", "fn descend(n:u64)->u64 { if n==0u64 {0u64} else {1u64+descend(n-1u64)} }\npub fn main()->u64 { let mut total=0u64; for n in 0u64..20u64 { total+=descend(n) } total }\n".to_owned()).unwrap();
        let output = source_check_request(&mut engine, file, &default_context()).unwrap();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let input = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(input).unwrap();
        for target in TargetSpec::executed() {
            let ready = {
 let options = &Options::new(target);
 hirsel_requests::source_ready_supplier(&"fn descend(n:u64)->u64 { if n==0u64 {0u64} else {1u64+descend(n-1u64)} }\npub fn main()->u64 { let mut total=0u64; for n in 0u64..20u64 { total+=descend(n) } total }\n", "counter.hir", &hirsel_engine::entry_path(&hirsel_engine::default_context()), &hirsel_native_testing::ready_toolchain(&options.target)).unwrap()(options)
}.unwrap();
            let ready = &ready.program;
            let ir = emit_ir(&ready, OptLevel::None).unwrap();
            let body = ir
                .lines()
                .skip_while(|line| {
                    !(line.starts_with("define")
                        && line.contains("7descend")
                        && !line.contains("@_H1T"))
                })
                .take_while(|line| *line != "}")
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!body.is_empty(), "{ir}");
            assert!(
                !body.contains("@hirsel_rt_v2_depth_enter_at_site("),
                "{body}"
            );
            assert!(!body.contains("@hirsel_rt_v2_depth_leave("), "{body}");
            assert!(body.contains("icmp uge i16"), "{body}");
            assert!(body.contains("store i16"), "{body}");
            let body = ir
                .lines()
                .skip_while(|line| {
                    !(line.starts_with("define")
                        && line.contains("4main")
                        && !line.contains("@_H1T"))
                })
                .take_while(|line| *line != "}")
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!body.is_empty(), "{ir}");
            assert!(body.matches("load i16").count() <= 2, "{body}");
        }
    });
}
