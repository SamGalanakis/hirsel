"""Build targets for the core script gates: M1, M2b, check cost and the SQLite native laws."""


def core_gates(out, call, Raw):
    out.append("\n" + call("hirsel_script_test", name="test-core-m1-runner", script="core/tools/test_m1.py",
                            data=["core/tools/m1.py", "core/tools/case_progress.py"]))
    # host: the installed-prefix case runs a program that opens a Dir, which needs the host's openat2.
    out.append("\n" + call("hirsel_script_test", name="core-library-modes", script="core/tools/gate_library_modes.py", host=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-library-modes/hirsel"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic",
                                       "HIRSEL_CORE_RT_ARCHIVES": "//:hirsel-core-rt-archives",
                                       "HIRSEL_LINK_KIT": "//tools/buck2/native-toolchain:kit",
                                       "HIRSEL_TOOLCHAIN_ARM": "//tools/buck2/native-toolchain:arm",
                                       "HIRSEL_TOOLCHAIN_QEMU": "//tools/buck2/native-toolchain:qemu"},
                            data=Raw('glob(["core/core/**", "core/std/**", "docs/spec/**"]) + ["core/tools/case_progress.py", "docs/PRIMER.md"]')))
    for engine in ("cranelift", "llvm"):
        # host: the files program's cases open a Dir, which needs the host's openat2.
        out.append("\n" + call("hirsel_script_test", name="core-m1-" + engine, script="core/tools/m1.py", host=True,
                                args=["--backend", engine, "--compiler", "buck-out/tmp/cargo-bin-exe/core-m1-" + engine + "/hirsel"],
                                bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                                files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                                data=Raw('glob(["core/core/**", "core/std/**", "core/programs/m1/**", "core/corpus/m1/**", "crates/hirsel-core-rt/include/**"]) + ["core/tools/case_progress.py"]')))
    # core-m2b-host runs the workloads with a spool directory on the invoking host.
    for name, host in (("core-m2b", False), ("core-m2b-host", True)):
        out.append("\n" + call("hirsel_script_test", name=name, script="core/tools/m2b.py",
                                **({"host": True} if host else {}),
                                args=["--compiler", "buck-out/tmp/cargo-bin-exe/" + name + "/hirsel",
                                      "--evaluator", "buck-out/tmp/cargo-bin-exe/" + name + "/hirsel-core-contract-eval"] + (["--host"] if host else []),
                                bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel",
                                          "hirsel-core-contract-eval": "//:hirsel-core-contract-eval-bin-hirsel-core-contract-eval"},
                                files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                                data=Raw('glob(["core/core/**", "core/std/**", "core/programs/m2b/**", "core/corpus/m2b/**", "crates/hirsel-core-rt/include/**"]) + ["core/front/cases/body/callable-generic-fit.hirsel", "core/front/cases/body/callable-static-mut.hirsel", "core/front/cases/body/type-recursion-finite.hirsel"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-recursive-instances", script="core/tools/recursive_instances.py", host=True, production=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-recursive-instances/hirsel"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic"},
                            data=Raw('glob(["core/core/**"]) + ["tools/capped"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-check-cost", script="crates/hirsel-core-front/check-cost.py", host=True, production=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-check-cost/hirsel", "--request-compiler", "buck-out/tmp/cargo-bin-exe/core-check-cost/hirsel-core-request-cost"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel", "hirsel-core-request-cost": "//:hirsel-core-driver-bin-hirsel-core-request-cost"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/m1/**"]) + ["core/tools/measure.py", "core/tools/m1.py", "tools/capped", "crates/hirsel-core-front/cost-ceilings.json"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-front-cost", script="crates/hirsel-core-front/front-cost.py", host=True, production=True, native_tools=True,
                            args=["--compiler", "buck-out/tmp/cargo-bin-exe/core-front-cost/hirsel", "--image-profiler", "buck-out/tmp/cargo-bin-exe/core-front-cost/hirsel-core-observed", "--buck-tool-inputs"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            front_observer_bin_exes={"hirsel-core-observed": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"HIRSEL_CORE_RT_ARCHIVES": "//:hirsel-core-rt-archives"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/m1/**", "core/langarena/**"]) + ["core/tools/request_cost.py", "core/tools/langarena.py", "core/tools/std_native.py", "core/tools/std_contracts.py", "tools/loopback_peer.py", "core/tools/m1.py", "core/tools/measure.py", "tools/capped", "crates/hirsel-core-front/check-cost.py", "crates/hirsel-core-front/front-cost-ceilings.json", "tools/native-toolchain/llvm-23.lock"] + ["core/tools/case_progress.py"]')))
    out.append("\n" + call("hirsel_script_test", name="core-check-cost-unit", script="crates/hirsel-core-front/test-check-cost.py", host=True,
                            data=["crates/hirsel-core-front/test-check-cost.py", "crates/hirsel-core-front/check-cost.py", "crates/hirsel-core-front/front-cost.py", "core/tools/request_cost.py", "core/tools/measure.py"]))
    # host: the -shm allocation law needs a 64 KiB tmpfs, which only the host launcher's bwrap mounts.
    out.append("\n" + call("hirsel_script_test", name="core-sqlite-native-laws", script="tools/tests/sqlite_native_laws.sh", host=True,
                            args=["buck-out/tmp/cargo-bin-exe/core-sqlite-native-laws/hirsel"],
                            bin_exes={"hirsel": "//:hirsel-core-driver-bin-hirsel"},
                            files_env={"SQLITE_HEADER": "//tools/buck2:sqlite-header[sqlite3/sqlite3.h]"},
                            extra_env={"HIRSEL_TEST_SMALL_TMPFS": "65536"},
                            data=Raw('glob(["core/core/**", "core/std/**", "core/programs/sqlite/**"])')))
