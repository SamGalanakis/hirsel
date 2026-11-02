hirsel_guard::install!();
include!("support/cpu_pool/physical.rs");
#[test]
fn fresh_cpu_pool_has_physical_width_16() {
    run_width(16);
}
include!("support/cpu_pool/replay.rs");
#[test]
fn wide_graph_batches_record_and_replay_after_worker_teardown() {
    replay_width(16);
}

include!("support/cpu_pool/jitter.rs");
#[test]
fn cpu_integer_float_bits_and_fresh_lifecycle_survive_jitter() {
    cpu_values_with_jitter(16);
}

include!("support/cpu_pool/generated.rs");
#[test]
fn free_generated_typed_batches_replay_with_one_permit() {
    generated_free_schedules(16);
}

include!("support/cpu_pool/watchdog.rs");

include!("support/cpu_pool/interpreter.rs");
include!("support/cpu_pool/cleanup_cuts.rs");
#[test]
fn native_wide_success_every_byte_cleanup_cut() {
    wide_cleanup_cuts(16, "success");
}
#[test]
fn native_wide_failure_every_byte_cleanup_cut() {
    wide_cleanup_cuts(16, "failure");
}
#[test]
fn native_wide_cancel_every_byte_cleanup_cut() {
    wide_cleanup_cuts(16, "cancel");
}

#[test]
fn cpu_job_withdrawal_and_transfer_retire_once_per_generation() {
    cpu_job_withdrawal(16);
}

include!("support/cpu_pool/canonical.rs");
#[test]
fn free_cpu_canonical_receipts_agree_across_emitters_and_replay() {
    cpu_free_canonical_receipts(16);
}

#[test]
#[ignore = "requires exclusive CPUs; tools/cpu-overlap reports host-blocked without counting a pass"]
fn ordered_allocating_mappers_overlap_and_replay() {
    run_width_policy_backends_mode(16, "tasks.cooperative@1", &["llvm", "cranelift"], true);
}
include!("support/cpu_pool/four_engine.rs");
#[test]
fn every_granted_width_and_bounded_job_count_matches_width_one() {
    cpu_fixture_four_engine_source(
        16,
        include_str!("support/cpu_pool/any_width.hir").into(),
        b"cpu-any-width-ok\n",
    );
}
