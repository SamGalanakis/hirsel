//! Source agreement through reached definition units and a C caller.
#![allow(dead_code)]
const NATIVE_TARGET: &str = "x86_64-unknown-linux-gnu";
hirsel_guard::install!();
include!("support/cranelift_source.rs");
#[test]
fn every_front_end_run_case_matches_evaluator() {
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

#[test]
fn blocked_channel_is_a_named_run_stop() {
    let source = r#"
fn waiting() -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = result.unwrap(channel.new(1))
    result.unwrap(channel.receive(rx))
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    assert_eq!(
        eval::run(&module, names["waiting"], &[], 1_000_000).outcome,
        Outcome::Stopped("Deadlock")
    );
    execute(&module, &[(names["waiting"], vec![])], false);
}
