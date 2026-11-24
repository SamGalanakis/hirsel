use super::{root, run_case};

#[test]
fn step_bounds_rules() {
    let mut failures = run_case(&root().join("cases/body/step-bounds.hirsel"));
    failures.extend(run_case(
        &root().join("../../crates/hirsel-core-rt/fixtures/order/step-bounds.hirsel"),
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn static_bytes_preserve_budget_law() {
    let failures = run_case(&root().join("cases/rules/MEM-28-static-bytes.hirsel"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
