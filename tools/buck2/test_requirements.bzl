"""Semantic CPU requirements of the surviving test harnesses."""

EXACT_TEST_CPUS = {}
EXACT_CASE_CPUS = {}
ISOLATED_TEST_CASES = {}

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
        "provider_tests::temp_basename_matches_the_completed_carrier_and_invalid_prefixes_do_not_create",
        "provider_tests::temp_collisions_retry_inside_issue_and_exhaustion_preserves_the_carrier",
    ],
    "hirsel-core-driver-unittest": [
        "native::tests::dir_cranelift_entry",
        "native::tests::dir_llvm_entry",
        "native::tests::host_operations_cranelift",
        "native::tests::host_operations_llvm",
        "native::tests::startup_refusals_cranelift",
        "native::tests::startup_refusals_llvm",
        "native::tests::cranelift_source_entries_link_run_and_reuse",
        "native::tests::llvm_source_entries_link_run_and_reuse",
    ],
}
