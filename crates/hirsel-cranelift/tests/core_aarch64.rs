//! AArch64 execution through the declared QEMU/sysroot and a real C caller.
#![allow(dead_code)] // The shared harness also serves the wider x86 fixture matrix.
const NATIVE_TARGET: &str = "aarch64-unknown-linux-gnu";
hirsel_guard::install!();
include!("support/native.rs");

#[test]
fn scalar_control_calls_and_aggregate_returns() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../hirsel-core-ir/samples/control.core"),
        include_str!("../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../hirsel-core-ir/samples/calls.core"),
    ] {
        let module = text::parse(source).unwrap();
        let runs = sample_runs(&module, source);
        assert!(!runs.is_empty());
        for optimize in [false, true] {
            execute(&module, &runs, optimize);
        }
    }
}

#[test]
fn front_end_run_cases() {
    check_front_end_run_cases();
}

#[test]
fn ownership_stable_counted_and_failure() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../hirsel-core-ir/samples/stable-keys.core"),
        include_str!("../../hirsel-core-ir/samples/sequences.core"),
        include_str!("../../hirsel-core-ir/samples/strings.core"),
        include_str!("../../hirsel-core-ir/samples/kernel.core"),
    ] {
        for optimize in [false, true] {
            check_sample_with_refusals(source, optimize);
        }
    }
}

#[test]
fn task_entry_stack_switching_and_custody() {
    for optimize in [false, true] {
        check_sample_with_refusals(
            include_str!("../../hirsel-core-ir/samples/custody.core"),
            optimize,
        );
    }
}
