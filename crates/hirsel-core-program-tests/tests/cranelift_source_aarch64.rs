//! AArch64 source agreement through production definition units.
#![allow(dead_code)]
const NATIVE: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;
hirsel_guard::install!();
include!("support/cranelift_source.rs");
#[test]
fn front_end_run_cases() {
    check_front_end_run_cases();
}

#[test]
fn failure_sites_in_dev_and_speed() {
    check_failure_sites();
}

#[test]
fn float_nan_paths_in_dev_and_speed() {
    check_float_nan_paths();
}

#[test]
fn growth_shadow_copies_back_data_and_capacity() {
    check_growth_shadow_words();
}
