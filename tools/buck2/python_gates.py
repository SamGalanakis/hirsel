"""Buck runners for Python tool unit suites."""


def python_gates(out, call, Raw):
    product = ["//tools/toolchain-dist:product.py", "//tools/toolchain-dist:product.toml"]
    out.append("\n" + call("hirsel_script_test", name="test-affected", script="tools/tests/affected.py", host=True,
                           data=["tools/affected"]))
    out.append("\n" + call("hirsel_script_test", name="test-board-freshness",
                           script="tools/tests/test_board_freshness.py", host=True,
                           data=["tools/board-environment.sh", "tools/board-freshness.py", "tools/hirsel-board.service",
                                 "tools/hirsel-board-alert.service", "tools/nightly-board.sh"]))
    out.append("\n" + call("hirsel_script_test", name="test-fmt", script="tools/tests/fmt.py", host=True,
                           data=["//tools/buck2:fmt.py"],
                           files_env={"HIRSEL_TEST_CASE_FORMATTER": "//:hirsel-core-syntax-bin-hirsel-core-syntax"}))
    out.append("\n" + call("hirsel_script_test", name="test-buck2-client", script="tools/tests/buck2_client.py",
                           data=["//tools/buck2:bootstrap.py", "//tools/buck2:bootstrap_store.py", "//tools/buck2:bootstrap_llvm.py",
                                 "//tools/buck2:client.py", "//tools/buck2:generated.py", "//tools/buck2:merging.py",
                                 "//tools/buck2:llvm-cxx-headers-lock.json",
                                 "//tools/buck2:pins.json", "tools/native-toolchain/llvm-23.lock", *product]))
    std_inputs = ["core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "core/tools/case_progress.py", "tools/loopback_peer.py"]
    python_units = [
        ("test-merging", "tools/tests/merging.py", ["//tools/buck2:merging.py"]),
        ("test-native-toolchain", "tools/tests/native_toolchain.py",
         ["//tools/buck2/native-toolchain:unpack-source", "tools/native-toolchain/engine_llvm.py",
          "core/tools/langarena.py", "core/tools/m1.py", "core/tools/m2b.py",
          "core/tools/measure.py", "core/tools/measurement_artifacts.py",
          "core/langarena/run.py", "core/langarena/layout/measure.py", "core/langarena/layout/run.py",
          "core/langarena/layout/service.py", "core/langarena/layout/service_build.py"] + std_inputs + product),
        ("test-prelude-overlay", "tools/tests/prelude_overlay.py", ["//tools/buck2:prelude_overlay.py"]),
        ("test-deb-unpack", "tools/tests/deb_unpack.py",
         ["//tools/buck2/native-toolchain:deb_unpack.py"]),
        ("test-bootstrap-llvm", "tools/tests/bootstrap_llvm.py", ["//tools/buck2:bootstrap_llvm.py", "//tools/buck2:bootstrap_store.py", "//tools/buck2:pins.json", "//tools/buck2:llvm-cxx-headers-lock.json", "tools/native-toolchain/llvm-23.lock", *product]),
        ("test-action-sizes", "tools/tests/action_sizes.py", ["//tools/buck2:action_sizes_from_log.py"]),
        ("test-pool-usage", "tools/tests/pool_usage.py", ["//tools/buck2:pool_usage.py", "//tools/buck2:event_logs.py", "//tools/buck2:action_sizes_from_log.py"]),
        ("test-sizes-from-usage", "tools/tests/test_sizes_from_usage.py", ["//tools/buck2:test_sizes_from_usage.py"]),
        ("test-check-target", "tools/test_check_target.py", ["tools/check_target.py", "tools/check_file_lengths.py", "//tools/buck2:defs.bzl"]),
        ("test-io-measure", "core/tools/test_io_measure.py", ["core/tools/io_measure.py", "core/tools/langarena.py",
                                                             "core/tools/measurement_artifacts.py"] + std_inputs),
        ("test-core-measurement-artifacts", "core/tools/test_measurement_artifacts.py", ["core/tools/measurement_artifacts.py"] + std_inputs),
        ("test-std-native", "core/tools/test_std_native.py",
         ["core/std/tests/io.hirsel", "core/tools/std_native_owners.json"] + std_inputs),
        ("test-boat-dispatch", "tools/test_boat_dispatch.py", ["tools/boat-dispatch", "tools/sim-campaign"]),
        ("test-store-chaos", "tools/store-chaos/test_harness.py",
         ["tools/store-chaos/history.py", "tools/store-chaos/log_writes.py",
          "tools/store-chaos/recover.py", "tools/store-chaos/relay.py"]),
    ]
    for target, script, inputs in python_units:
        out.append("\n" + call("hirsel_script_test", name=target, script=script, data=inputs))
    # These suites use a local checkout, Git, host paths or host CPU affinity.
    for target, script in [
        ("test-capped", "tools/tests/capped.py"),
        ("test-event-logs", "tools/tests/event_logs.py"),
        ("test-check-file-lengths", "tools/test_check_file_lengths.py"),
        ("test-check-boundaries", "tools/test_check_boundaries.py"),
        ("test-board-history", "tools/tests/test_board_history.py"),
        ("test-ignored-tests", "tools/tests/ignored_tests.py"),
        ("test-vopr-failure-groups", "core/tools/test_vopr_failure_groups.py"),
        ("test-vopr-nightly-receipt", "core/tools/test_vopr_nightly_receipt.py"),
        ("test-store-cross-build", "core/tools/test_store_cross_build.py"),
        ("test-store-ace", "core/tools/test_store_ace.py"),
        ("test-board-bisect", "tools/tests/test_board_bisect.py"),
        ("test-hot-files", "tools/tests/hot_files.py"),
        ("bench-awfy-measure", "bench/awfy/test_measure.py"),
        ("bench-awfy-receipt", "bench/awfy/test_receipt.py"),
        ("bench-awfy-weekly", "bench/awfy/test_weekly.py"),
        ("test-langarena", "core/langarena/test_run.py"),
        ("test-langarena-peak", "core/langarena/layout/test_peak.py"),
        ("test-langarena-service-build", "core/langarena/layout/test_service_build.py"),
    ]:
        out.append("\n" + call("hirsel_script_test", name=target, script=script, host=True, data=[]))
    out.append("\n" + call("hirsel_script_test", name="test-obs-export", script="tools/tests/obs_export.py",
                            data=Raw('glob(["tools/tests/fixtures/obs-export/*.json"]) + ["tools/obs_export.py", "tools/loopback_peer.py", "core/std/tests/vectors/trace/two_spans.json"]')))
    out.append("\n" + call("hirsel_script_test", name="bench-micro-http-instructions", script="bench/micro/http/test_instructions.py",
                            data=Raw('glob(["bench/micro/http/**"])')))
