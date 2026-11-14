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

#[test]
fn mixed_float_and_large_returns_through_indirect_calls() {
    check_c_struct_calls();
}

#[test]
fn kernel_finalizers() {
    // This sample's hand-built ring assumes its buffer allocation succeeds.
    // Keep the existing ordinary-case oracle; refusal-safe samples are above.
    let source = include_str!("../../hirsel-core-ir/samples/kernel.core");
    let module = text::parse(source).unwrap();
    for optimize in [false, true] {
        execute(&module, &sample_runs(&module, source), optimize);
    }
}

#[test]
fn failure_sites_in_dev_and_speed() {
    check_failure_sites();
}

#[test]
fn relative_debug_rows_and_distinct_cold_sites() {
    check_relative_debug_rows();
}

#[test]
fn float_nan_paths_in_dev_and_speed() {
    check_float_nan_paths();
}

#[test]
fn consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    for optimize in [false, true] {
        check_sample_with_refusals(
            include_str!("../../hirsel-core-rt/testdata/grow.core"),
            optimize,
        );
    }
}

#[test]
fn consuming_growth_host_memory_refusal_is_a_reported_run_stop() {
    host_memory::check_aarch64(
        |module| {
            emit_object_with_layouts(
                module,
                &hirsel_core_layout::compute(&module.types).unwrap(),
                &Options {
                    target: NATIVE_TARGET,
                    optimize: true,
                    debug_lines: false,
                },
            )
            .unwrap()
            .bytes
        },
        |module, function| {
            symbol(&module.instances[module.functions[function.index()].instance.index()])
        },
    );
}
