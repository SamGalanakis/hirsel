"""Semantic CPU requirements of the surviving test harnesses."""

# These laws execute sealed memfds. The native host supports them, but the
# bwrap profile used for loopback isolation denies execveat on a memfd.
NATIVE_EXEC_TEST_CASES = [
    "provider_tests::exec_supervisor_preserves_inputs_and_only_reaps_its_children",
    "provider_tests::exec_provider_runs_the_sealed_image_and_uses_directional_pipes",
]

# These bodies use the real Linux OS Dir provider. Compile on the pool, execute
# only on the invoking host; each body executes the shared fail-loud precondition.
OPENAT2_TEST_CASES = {
    "hirsel-core-rt-unittest": [
        "entry_tests::c_entry_constructor_fills_main_order_with_u64_handles_and_copies_argv",
        "host_file_tests::native_host_file_exit_paths_close_once_and_open_admits_nothing",
        "host_file_tests::run_stop_skips_host_file_cleanup_and_kernel_releases_the_descriptor",
        "host_ops_tests::every_host_op_routes_to_the_host_and_records_delivery",
        "provider_tests::files_deliver_committed_prefixes_and_all_open_modes",
        "provider_tests::rule_provider_tests::cap7_temporary_dir_never_widens_its_parent_authority",
        "provider_tests::guarded_root_routes_real_os_calls_through_the_separate_host_stack",
        "provider_tests::locks_are_exclusive_idempotent_and_end_at_close",
        "provider_tests::paths_cannot_escape_grants_and_tree_drop_never_follows_links",
        "provider_tests::unix_endpoints_retain_authority_and_datagrams_commit_exactly_one_packet",
        "host_ops_tests::sleep_until_cancellation_is_cancelled_at_the_native_wait_boundary",
        "host_ops_tests::live_sleep_until_waits_on_host_clock_and_advances_wall",
        "host_ops_tests::already_reached_sleep_until_returns_at_once",
        "provider_tests::temp_basename_matches_the_completed_carrier_and_invalid_prefixes_do_not_create",
        "provider_tests::temp_collisions_retry_inside_issue_and_exhaustion_preserves_the_carrier",
        "provider_tests::rename_follows_each_sentence_of_core_md",
        "provider_s0_tests::s0_os_callbacks_by_identity_match_the_pointer_calls",
        "tape::tests::every_s1_kind_recorded_on_the_os_replays_with_no_host",
        "tape::tests::filesystem_observations_survive_removing_the_recorded_directory",
    ] + NATIVE_EXEC_TEST_CASES,
    "hirsel-core-driver-unittest": [
        "host::child::terminal_fixture::terminal_bits_follow_each_granted_descriptor",
        "native::tests::dir_cranelift_entry",
        "native::tests::dir_llvm_entry",
        "native::tests::host_operations_cranelift",
        "native::tests::host_operations_llvm",
        "native::tests::prog3_startup_refusals_cranelift",
        "native::tests::prog3_startup_refusals_llvm",
        "native::tests::prog3_prog6_cranelift_source_entries_link_run_and_reuse",
        "native::tests::prog3_prog6_llvm_source_entries_link_run_and_reuse",
        "native::tests::tape_run_replays_cranelift",
        "native::tests::tape_run_replays_llvm",
        "native::tests::run_tape_replays_stdin_and_env_without_host_sources",
        "native::tests::run_tape_replays_granted_terminal_bits",
        "native::tests::run_tape_records_scram_outcome_without_key_material",
        "native::tests::tape_code_revision_is_the_entry_revision",
        "native::tests::tape_http_replays_both_native_engines",
        "native::tests::rl3_replays_tls_offline",
        "native::tests::a_program_over_a_separate_std_records_what_it_records_whole",
        "native::tests::rl4_rl5_replays_tls_without_secret_sources",
        "native::tests::simd_versions_preserve_run_tape_and_revision",
        "native::tests::rl1_recording_changes_nothing",
        "native::tests::rl2_divergence_ends_replay_at_position",
        "native::tests::test_replay_flag_records_and_replays",
        "native::tests::served_scram_replays_with_the_same_send_calls",
        "native::tests::recording_cli_replays",
        "native::tests::recording_schedule_replays_across_engines",
        "native::tests::parallel_test_cli_preserves_rows_and_isolates_shared_directory",
        "native::tests::sqlite_directory_recovery_locking_and_sync",
        "native::tests::recording_switch_preserves_native_observations",
    ],
}
