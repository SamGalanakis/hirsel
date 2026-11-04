//! A nonfinal release of a slice owner needs only its task-local count word.
use hirsel_engine::{
    QueryEngine, default_context, default_environment, source_check_request, with_compiler_stack,
};
use hirsel_front_phases::compiler_adapters;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::TargetSpec;
use hirsel_native::emit_ir;
use hirsel_native_batch::build;

#[test]
fn erased_owners_decrement_shared_counts_before_the_runtime_fallback() {
    for target in [
        TargetSpec::x86_64_unknown_linux_gnu(),
        TargetSpec::aarch64_unknown_linux_gnu(),
    ] {
        with_compiler_stack(move || {
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let source = "fn consume(take data: Bytes) -> () {}
pub fn main() -> Result[(), ()] { consume(b\"a\")
  Ok(()) }
";
            let file = engine
                .sources
                .add("release.hir", source.to_owned())
                .unwrap();
            let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
            assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
            let input = engine.export_native_input().unwrap();
            let validated = engine.import_native_input(input).unwrap();
            let ready = build(&validated, &Options::new(target)).unwrap();
            let ir = emit_ir(&ready, OptLevel::Speed).unwrap();
            let body = ir
                .lines()
                .skip_while(|line| {
                    !(line.starts_with("define ")
                        && line.contains("7consume")
                        && !line.contains("@_H1T"))
                })
                .take_while(|line| *line != "}")
                .collect::<Vec<_>>()
                .join("\n");
            assert!(body.contains("@hirsel_rt_v2_release("), "{body}");
            assert!(
                body.contains("load i32,") && body.contains("store i32"),
                "shared releases must use the count word before the final-credit fallback\n{body}"
            );
        });
    }
}
