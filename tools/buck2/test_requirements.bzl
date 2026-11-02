"""Semantic CPU requirements, independent of measured resource usage.

These harnesses assert physical worker widths. Exact counts override measured
CPUs and focus mode; measured memory and CPU class remain authoritative.
"""

EXACT_TEST_CPUS = {
    "gate-langarena": 16,  # quick checksums include Matmul::T16
    "hirsel-cli-test-cpu_matmul_4": 4,
    "hirsel-cli-test-cpu_matmul_8": 8,
    "hirsel-cli-test-cpu_matmul_16": 16,

    "hirsel-cli-test-cpu_pool_1": 1,
    "hirsel-cli-test-cpu_pool_4": 4,
    "hirsel-cli-test-cpu_pool_8": 8,
    "hirsel-cli-test-cpu_pool_16": 16,
    "hirsel-cli-test-free": 8,  # F1 actual-eight admission and replay matrices
}

# Per-case widths for binaries whose cases differ: a case listed here asserts
# that many physical CPUs (a capped run under HIRSEL_JOBS=N, placement proofs,
# effective-width assertions); every other case of the binary runs under its
# measured request. Sharding groups the listed cases by width into exact shards
# (`<binary>-cpus-N-shard-i`), and the test launcher refuses to run a case whose
# width exceeds its action's CPUs. A binary is listed here or in EXACT_TEST_CPUS,
# never both.
EXACT_CASE_CPUS = {
    "hirsel-cli-test-cli": {
        "chunk_backup_31::chunk_backup_31_scripted_native_record_replay": 4,
        "cron_state_30::cron_state_30_numeric_step_native_and_tapes": 4,
        "cron_state_30::cron_state_30_ordered_cranelift": 4,
        "cron_state_30::cron_state_30_ordered_llvm": 4,
        "cron_state_30::cron_state_30_past_deadline_native_replay": 4,
        "depsolve_29::depsolve_29_native_llvm_bounds": 4,
        "depsolve_29::depsolve_29_native_llvm_workflows": 4,
        "durable_queue_31::durable_queue_31_fixture_native_live": 4,
        "e1_live_hang_29::dagbuild_diamond_live_matches_the_oracle_repeatedly_at_each_width": 4,
        "e1_live_hang_29::process_diamond_live_finishes_repeatedly_at_each_width": 4,
        "host_replay_parity_31::task_set_signal_suspensions_replay_exact_readiness": 4,
        "hostcap_result_export_38::hostcap_result_export_38_nested_resource_result_and_replay": 4,
        "hostcap_result_export_38::hostcap_result_export_38_resource_result_and_replay": 4,
        "hostcap_result_export_38::hostcap_result_export_38_variant_resource_result_and_replay": 4,
        "journal_kv_29::journal_kv_29_native_crash_helpers": 4,
        "journal_kv_29::journal_kv_29_open_append_native_replay": 4,
        "man_render_30::man_render_30_host_refusal_controls": 4,
        "packtree_29::packtree_29_native_workflows_replay": 4,
        "pnm_tiles_30::pnm_tiles_30_replay_refusals": 4,
        "pubsub_relay_30_cells::admission_cap_llvm_1": 4,
        "pubsub_relay_30_cells::admission_cap_llvm_2": 4,
        "pubsub_relay_30_cells::denied_llvm_1": 4,
        "pubsub_relay_30_cells::denied_llvm_2": 4,
        "pubsub_relay_30_cells::duplicate_llvm_1": 4,
        "pubsub_relay_30_cells::duplicate_llvm_2": 4,
        "pubsub_relay_30_cells::duration_over_llvm_1": 4,
        "pubsub_relay_30_cells::duration_over_llvm_2": 4,
        "pubsub_relay_30_cells::frame_cap_llvm_1": 4,
        "pubsub_relay_30_cells::frame_cap_llvm_2": 4,
        "pubsub_relay_30_cells::host_cancel_cranelift_4": 4,
        "pubsub_relay_30_cells::host_cancel_llvm_1": 4,
        "pubsub_relay_30_cells::host_cancel_llvm_4": 4,
        "pubsub_relay_30_cells::host_domain_cranelift_4": 4,
        "pubsub_relay_30_cells::host_domain_llvm_1": 4,
        "pubsub_relay_30_cells::host_domain_llvm_4": 4,
        "pubsub_relay_30_cells::host_success_cranelift_4": 4,
        "pubsub_relay_30_cells::host_success_llvm_1": 4,
        "pubsub_relay_30_cells::host_success_llvm_4": 4,
        "pubsub_relay_30_cells::jobs_over_llvm_1": 4,
        "pubsub_relay_30_cells::jobs_over_llvm_2": 4,
        "pubsub_relay_30_cells::jobs_zero_llvm_1": 4,
        "pubsub_relay_30_cells::jobs_zero_llvm_2": 4,
        "pubsub_relay_30_cells::late_subscribe_llvm_1": 4,
        "pubsub_relay_30_cells::late_subscribe_llvm_2": 4,
        "pubsub_relay_30_cells::malformed_cranelift_1": 4,
        "pubsub_relay_30_cells::malformed_llvm_1": 4,
        "pubsub_relay_30_cells::malformed_llvm_2": 4,
        "pubsub_relay_30_cells::no_subscriber_llvm_1": 4,
        "pubsub_relay_30_cells::no_subscriber_llvm_2": 4,
        "pubsub_relay_30_cells::one_receiver_llvm_1": 4,
        "pubsub_relay_30_cells::one_receiver_llvm_2": 4,
        "pubsub_relay_30_cells::order_two_llvm_1": 4,
        "pubsub_relay_30_cells::order_two_llvm_2": 4,
        "pubsub_relay_30_cells::oversize_llvm_1": 4,
        "pubsub_relay_30_cells::oversize_llvm_2": 4,
        "pubsub_relay_30_cells::pending_read_cancel_cranelift_1": 4,
        "pubsub_relay_30_cells::pending_read_cancel_llvm_1": 4,
        "pubsub_relay_30_cells::pending_read_cancel_llvm_2": 4,
        "pubsub_relay_30_cells::pending_read_cancel_llvm_4": 4,
        "pubsub_relay_30_cells::truncated_body_llvm_1": 4,
        "pubsub_relay_30_cells::truncated_body_llvm_2": 4,
        "pubsub_relay_30_cells::truncated_header_llvm_1": 4,
        "pubsub_relay_30_cells::truncated_header_llvm_2": 4,
        "pubsub_relay_30_cells::two_receivers_cranelift_1": 4,
        "pubsub_relay_30_cells::two_receivers_llvm_1": 4,
        "pubsub_relay_30_cells::two_receivers_llvm_2": 4,
        "pubsub_relay_30_cells::unsubscribe_llvm_1": 4,
        "pubsub_relay_30_cells::unsubscribe_llvm_2": 4,
        "relation_query_33::relation_query_33_reference_checked_vectors": 4,
        "replay_scripted_net_30::unanswered_then_refused_replays_without_clock_deadline_or_queue": 4,
        "replay_select_pending_27::connect_only": 4,
        "replay_select_pending_27::deadline": 4,
        "replay_select_pending_27::write_read": 4,
        "rpc_gateway_30::rpc_gateway_30_disconnect_during_observed_backoff": 4,
        "schema_etl_30::schema_etl_30_native_host_and_four_engine_replay": 4,
        "shardsearch_27::shardsearch_27_reference_and_checked_application": 4,
        "sheet_eval_31::sheet_eval_31_afterwrite_full_entry": 4,
        "sheet_eval_31::sheet_eval_31_bounded_stdin_original_oracles": 4,
        "sheet_eval_31::sheet_eval_31_canceled_recalculation": 4,
        "sheet_eval_31::sheet_eval_31_native_additional_providers": 4,
        "sheet_eval_31::sheet_eval_31_native_full_entry_replay": 4,
        "sheet_eval_31::sheet_eval_31_scripted_ordered_widths": 4,
        "sitebuild_29::sitebuild_29_native_bounds": 4,
        "sitebuild_29::sitebuild_29_native_workflows": 4,
        "spectral_audit_35::spectral_audit_35_scripted_ordered_streams": 4,
        "suite_runner_29::suite_runner_29_native_and_host_workflows": 4,
        "text_index_30::text_index_30_native_boundaries": 4,
        "text_index_30::text_index_30_native_workflows_and_host_replay": 4,
        "toy_analyzer_31::toy_analyzer_31_incremental_native_and_missing_vectors": 4,
        "toy_analyzer_31::toy_analyzer_31_ready_release_affected": 4,
        "toy_analyzer_31::toy_analyzer_31_remaining_native_cache_and_session": 4,
        "toy_analyzer_31::toy_analyzer_31_scripted_native_and_four_engine_replay": 4,
        "toy_analyzer_31::toy_analyzer_31_scripted_width_streams": 4,
        "trace_record_27::trace_record_27_argument_reads_and_deadline_scopes_preserve_canonical_evidence": 4,
        "trace_resource_tape_55::a_shared_file_sink_replays_its_recorded_resource_graph": 4,
        "trace_resource_tape_55::stdio_aliases_replay_their_recorded_resource_graph": 4,
        "treepatch_29::treepatch_29_host_native_and_replay": 4,
        "unix_listen_replay_31::host_stale_denied_replays_at_each_width": 4,
        "unix_listen_replay_31::host_stale_granted_replays_at_each_width": 4,
        "unix_listen_replay_31::script_listen_replays_at_each_width": 4,
        "unix_listen_replay_31::script_remove_stale_replays_at_each_width": 4,
        "width_capacity_36::fixture_placement_starts_exact_workers": 4,
        "width_capacity_36::immediate_host_records_exact_admission_and_replays": 4,
    },
}

# The deadline replay matrix's cells (hirsel_deadline_replay_matrix): a native
# cell's workers run on as many physical CPUs as its width; the reference and
# width-1 cells run under the binary's measured request (0).
REPLAY_TEST_BINARY = "hirsel-cli-test-cli"
REPLAY_CELL_CPUS = {"reference": 0, "1": 0, "2": 2, "4": 4}

def _check_requirements():
    for binary in EXACT_CASE_CPUS:
        if binary in EXACT_TEST_CPUS:
            fail("{} declares both a binary width and case widths".format(binary))

_check_requirements()

# Keep indivisible slow bodies inside the same action cap without charging an
# aggregate's other bodies for them. These retain their compiled identities.
ISOLATED_TEST_CASES = {
    "hirsel-driver-test-it-stdlib": ["stdlib_uri::zig_file"],
}
