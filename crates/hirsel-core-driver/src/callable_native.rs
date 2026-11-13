// The shared core-source harness checks and runs the callable cases without a
// second fixture core or a separate runtime convention.
fn captured_formal_regions(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-region-captured.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn captured_formal_regions_cranelift(){captured_formal_regions(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn captured_formal_regions_llvm(){captured_formal_regions(Backend::Llvm);}
