"""Build targets for the Runner contract and wire checks."""


# A build of a core/programs member resolves the whole workspace: every member's
# manifest and listed sources, and the lock.
WORKSPACE = '"core/programs/hirsel.hcfg", "core/programs/hirsel.lock", "core/programs/*/hirsel.hcfg", "core/programs/**/*.hirsel"'


def runner_gates(out, call, Raw):
    out.append("\n" + call("hirsel_script_test", name="core-sdk-recovery", script="core/tools/sdk_recovery.py",
                            host=True, native_tools=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-sdk-recovery/hirsel",
                                  "--node", "buck-out/tmp/cargo-bin-exe/core-sdk-recovery/node"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel",
                                      "node": "//tools/buck2:sdk-node[bin/node]"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel", "core/programs/runner/tests/client*.mjs", "core/programs/runner/tests/client_recovery_place.hirsel", "sdk/client/*.ts"]) + ["core/programs/store/hirsel.hcfg", "core/programs/runner/hirsel.hcfg", "core/tools/store_tests.py", "core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "core/tools/case_progress.py", "tools/loopback_peer.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-runner-frame-schema", script="core/tools/frame_schema.py",
                            production=True, native_tools=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-runner-frame-schema/hirsel", "--check"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel"]) + ["core/programs/store/hirsel.hcfg", "core/programs/runner/hirsel.hcfg", "core/programs/runner/frame-schema.bin", "core/tools/store_tests.py", "core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "core/tools/case_progress.py", "tools/loopback_peer.py"]')))
    # R1 exercises durable calls, runs-once and store refusal across the evaluator
    # and native engines. R0 includes crash cuts.
    out.append("\n" + call("hirsel_script_test", name="core-runner-r1", script="core/tools/runner_tests.py",
                            host=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-runner-r1/hirsel",
                                  "--evaluator", "buck-out/tmp/cargo-bin-exe/core-runner-r1/hirsel-core-contract-eval", "--part", "r1"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel",
                                      "hirsel-core-contract-eval": "//:hirsel-core-contract-eval-bin-hirsel-core-contract-eval"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic",
                                       "HIRSEL_CORE_RT_SCRIPTED_ARCHIVE": "//:core-scripted-runtime-pic",
                                       "HIRSEL_CORE_RT_OBSERVED_ARCHIVES": "//:hirsel-core-rt-observed-archives",
                                       "HIRSEL_STORE_CRASH_BRIDGE": "//:core-store-crash-provider[shared]"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel", "core/programs/platform/*.hirsel", "core/programs/store-crash/*.hirsel", "core/programs/jwtbind/*.hirsel", "crates/hirsel-core-rt/include/**"]) + ["core/tools/runner_host.py", "core/tools/operator_fixture.py", "tools/tests/fixtures/host-admission.hcfg", "core/tools/runner_rest.py", "core/programs/hirsel.hcfg", "core/programs/store/hirsel.hcfg", "core/programs/runner/hirsel.hcfg", "core/programs/platform/hirsel.hcfg", "core/programs/store-crash/hirsel.hcfg", "core/programs/jwtbind/hirsel.hcfg", "core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "tools/loopback_peer.py", "core/tools/store_tests.py", "core/tools/runner_count.c", "core/tools/store_crash_tests.py", "core/tools/runner_crash.c"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-runner-r0", script="core/tools/runner_tests.py",
                            host=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-runner-r0/hirsel",
                                  "--evaluator", "buck-out/tmp/cargo-bin-exe/core-runner-r0/hirsel-core-contract-eval", "--part", "r0"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel",
                                      "hirsel-core-contract-eval": "//:hirsel-core-contract-eval-bin-hirsel-core-contract-eval"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic",
                                       "HIRSEL_CORE_RT_OBSERVED_ARCHIVES": "//:hirsel-core-rt-observed-archives",
                                       "HIRSEL_STORE_CRASH_BRIDGE": "//:core-store-crash-provider[shared]"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel", "core/programs/platform/*.hirsel", "core/programs/store-crash/*.hirsel", "core/programs/jwtbind/*.hirsel", "crates/hirsel-core-rt/include/**"]) + ["core/tools/runner_host.py", "core/tools/operator_fixture.py", "tools/tests/fixtures/host-admission.hcfg", "core/tools/runner_rest.py", "core/programs/hirsel.hcfg", "core/programs/store/hirsel.hcfg", "core/programs/runner/hirsel.hcfg", "core/programs/platform/hirsel.hcfg", "core/programs/store-crash/hirsel.hcfg", "core/programs/jwtbind/hirsel.hcfg", "core/programs/runner/frame-vectors.json", "core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "tools/loopback_peer.py", "core/tools/store_tests.py", "core/tools/runner_count.c", "core/tools/runner_mark.c", "core/tools/store_crash_tests.py", "core/tools/store_crash.c", "core/tools/runner_crash.c"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-runner-wire", script="core/tools/runner_tests.py",
                            host=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-runner-wire/hirsel",
                                  "--evaluator", "buck-out/tmp/cargo-bin-exe/core-runner-wire/hirsel-core-contract-eval", "--part", "wire"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel",
                                      "hirsel-core-contract-eval": "//:hirsel-core-contract-eval-bin-hirsel-core-contract-eval"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                            data=Raw('glob(["core/core/**", "core/std/**", "crates/hirsel-core-rt/include/**", ' + WORKSPACE + ']) + ["core/tools/runner_host.py", "core/tools/operator_fixture.py", "tools/tests/fixtures/host-admission.hcfg", "core/tools/runner_rest.py", "core/programs/runner/frame-vectors.json", "core/tools/std_native.py", "core/tools/contract_exec.py", "core/tools/std_contracts.py", "tools/loopback_peer.py", "core/tools/store_tests.py", "core/tools/store_crash_tests.py", "core/tools/std_socket.c", "tools/tests/wire_build.py", "tools/tests/wire_admission.py", "tools/tests/wire_wsock.py", "tools/tests/wire_process.py"] + ["core/tools/case_progress.py"]')))
    # The wire fixtures (core/programs/wire) and their loopback checks.
    out.append("\n" + call("hirsel_script_test", name="core-wire-fixtures", script="tools/tests/wire_gate.py",
                            host=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-wire-fixtures/hirsel"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                            data=Raw('glob(["core/core/**", "core/std/**", ' + WORKSPACE + ']) + ["tools/tests/wire_gate.py", "tools/tests/wire_build.py", "tools/tests/wire_wsock.py", "tools/tests/wire_finish.py", "tools/tests/wire_admission.py", "tools/tests/wire_process.py", "tools/loopback_peer.py", "core/tools/std_socket.c"]')))
