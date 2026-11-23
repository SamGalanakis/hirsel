"""First-party Rust targets, as tools/buck2/sync.py writes them into the root BUCK.

Every first-party target is in the root package, so `srcs` keep their
repository paths inside a compile action and relative `include_str!` paths
resolve as they do under Cargo.

Tests run as Cargo runs them, from their package's directory. Every compiled
path Cargo makes absolute is relative to that directory instead (set with
rustc's `--env-set`, which wins over the absolute sandbox path the prelude
would give): `env!("CARGO_MANIFEST_DIR")` is `../../crates/<crate>`, which
names the crate from any package directory, locally or on the pool, and so do
CARGO_TARGET_TMPDIR and CARGO_BIN_EXE_<name>.
"""

load(":platforms.bzl", "CLIPPY_REQUEST", "PRECHECK_REQUEST", "PROBE_CPUS", "budget_constraint", "compile_request", "doctest_properties", "dist_compile_request", "pool_budget_name", "test_budget_name", "test_executor", "test_request", "test_memory_limit")
load(":test_requirements.bzl", "HOST_TEST_BINARIES", "NATIVE_EXEC_TEST_CASES", "OPENAT2_TEST_CASES", "PERF_TEST_CASES")
load(":exec_sizes.bzl", "COMPILE_CLASSES", "COMPILE_REQUESTS")
load(":profile.bzl", "runtime_profile_flags", "workspace_profile_flags")
load("@prelude//rust:cargo_package.bzl", "get_reindeer_platforms")
load("@prelude//utils:selects.bzl", "selects")
load("@prelude//linking:link_info.bzl", "ArchiveLinkable", "LinkStrategy", "MergedLinkInfo", "ObjectsLinkable")
load("@prelude//rust:link_info.bzl", "RustLinkInfo", "dfs_dedupe_by_label")

def cargo_platforms(values):
    """Use the same target configuration names as Reindeer's external rules."""
    default = [] if type(values.values()[0]) == "list" else None
    return selects.apply(get_reindeer_platforms(), lambda platform: values.get(platform, default))

# Cargo gives integration tests an existing, private scratch directory; this is
# its repository-relative stand-in, created by the test launcher.
TEST_TMPDIR = "buck-out/tmp/cargo-target-tmpdir"

# An integration test's CARGO_BIN_EXE_<name> is compiled in as
# `<BIN_EXE_DIR>/<harness>/<name>`, which the test launcher links to the
# binary the test run carries. With `$(location)` in the harness's flags each
# binary was an input of the harness compile: one harness link waited for
# five whole-compiler links and fetched their 974 MB on its worker, though
# rustc reads only the path.
BIN_EXE_DIR = "buck-out/tmp/cargo-bin-exe"

# `-c hirsel.tests=host` explicitly opts into execution on the invoking host;
# `-c hirsel.tests=focus` uses one CPU unless a target requires an exact count.
TESTS_ON_HOST = read_root_config("hirsel", "tests", "pool") == "host"
def _request(binary):
    return [str(field) for field in test_request(binary)]

def _test_platform(binary):
    """The invoking host, or the measured pool request for `binary`."""
    return [budget_constraint("host" if TESTS_ON_HOST or binary in HOST_TEST_BINARIES else test_budget_name(binary))]

def _from_package(manifest_dir, path):
    """`path`, relative to the repository root, as seen from `manifest_dir`."""
    return "../" * len(manifest_dir.split("/")) + path

def _cargo_env(package_name, crate, version):
    numbers = version.split("-")[0].split(".")
    return {
        "CARGO_CRATE_NAME": crate,
        "CARGO_PKG_NAME": package_name,
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_VERSION_MAJOR": numbers[0],
        "CARGO_PKG_VERSION_MINOR": numbers[1],
        "CARGO_PKG_VERSION_PATCH": numbers[2],
        # Clippy looks for clippy.toml from CLIPPY_CONF_DIR, else
        # CARGO_MANIFEST_DIR, upwards. The workspace has none, so the action's
        # root gives Clippy's defaults, as under Cargo; compiled code sees the
        # `--env-set` CARGO_MANIFEST_DIR.
        "CLIPPY_CONF_DIR": ".",
    }

# A first-party binary finds the engine libLLVM (llvm-files/engine, the lock's
# `engine_llvm`) and then the libraries bundled for the pinned tree
# (llvm-files/lib: the engine's libz) in the tree's source directory, from its
# own buck-out location (buck-out/v2/<art|gen>/<cell>/<configuration>/__<target>__/<binary>),
# also when a test runs it with a cleared environment. DT_RPATH, unlike
# DT_RUNPATH, also serves libLLVM's own dependencies. Tests on the pool carry
# both directories (toolchains//:llvm-libraries).
LLVM_RPATH = [
    "-Clink-arg=-Wl,--disable-new-dtags",
    "-Clink-arg=-Wl,-rpath,$ORIGIN/../../../../../../tools/buck2/toolchains/llvm-files/engine:$ORIGIN/../../../../../../tools/buck2/toolchains/llvm-files/lib",
]

# Line tables were 61% of a whole-compiler executable (128 of 210 MB), which
# every link uploads to the cache and every test run fetches. lld compresses
# them; std's backtraces read zlib sections (zstd ones lose file and line), so
# failing tests keep their locations.
# The driver's compiler identity is its linker build ID (reuse.rs
# compiler_identity); without the note it hashes the whole executable on every
# request. The bootstrap clang does not pass --build-id by default.
EXECUTABLE_LINK_FLAGS = LLVM_RPATH + [
    "-Clink-arg=-Wl,--compress-debug-sections=zlib",
    "-Clink-arg=-Wl,--build-id=sha1",
]

def _env_set(key, value):
    return "--env-set={}={}".format(key, value)

def _compile_constraints(name):
    return select({
        "root//tools/buck2:profile_release": [budget_constraint(pool_budget_name(*compile_request("release", name)))],
        "root//tools/buck2:profile_dist": [budget_constraint(pool_budget_name(*dist_compile_request(name)))],
        "root//tools/buck2:profile_runtime": [budget_constraint(pool_budget_name(*compile_request("release", name)))],
        "root//tools/buck2:profile_profiling": [budget_constraint(pool_budget_name(*compile_request("release", name)))],
        "DEFAULT": [budget_constraint(pool_budget_name(*compile_request("dev", name)))],
    })

def _release_codegen_units(name):
    # A measured release compile's codegen units match its reservation, and a
    # one-CPU compile keeps two: its slot share covers them
    # (action_sizes_from_log.py). Unmeasured targets keep Cargo's defaults.
    parts = name.split("-features-")
    name = parts[0] + parts[1][12:] if len(parts) == 2 else name
    # Core request measurements favor one unit: all 18 product requests save
    # 2.92-4.17K instructions/line; both all-source controls improve. This
    # release-only exception retains the measured compile reservations.
    # The four support libraries add 0.05-0.15K savings per product line.
    if name in ["hirsel-core-front", "hirsel-core-driver", "hirsel-core-materialize", "hirsel-core-ir", "hirsel-core-syntax", "hirsel-core-resolve", "hirsel-core-ids", "hirsel-core-layout"]:
        return ["-Ccodegen-units=1"]
    if name not in COMPILE_REQUESTS["release"] and not (PROBE_CPUS and name in COMPILE_CLASSES["release"]):
        return []
    return ["-Ccodegen-units={}".format(max(2, compile_request("release", name)[0]))]

# A dev compile splits its code into 16 units, Cargo's non-incremental default,
# whatever its reservation: LLVM then spreads the units over the reserved CPUs
# instead of waiting on the largest of two to four. A compile that reserves
# more than one CPU also runs rustc's front end on that many threads. On types
# edits, alternated with the reservation-sized units (median execution): the rc
# rlib 37 -> 20 s, validate 48 -> 35 s, lir 30 -> 22 s, types 15 -> 8.5 s, the
# driver `it` harness 14.5 -> 10 s, rc metadata 8 -> 4 s.
DEV_CODEGEN_UNITS = 16

def _dev_codegen(name):
    cpus = compile_request("dev", name)[0]
    return ["-Ccodegen-units={}".format(DEV_CODEGEN_UNITS)] + (["-Zthreads={}".format(cpus)] if cpus > 1 else [])

def _codegen_flags(name):
    return select({
        "root//tools/buck2:profile_release": _release_codegen_units(name),
        "root//tools/buck2:profile_dist": [],
        "root//tools/buck2:profile_runtime": [],
        "root//tools/buck2:profile_profiling": _release_codegen_units(name),
        "DEFAULT": _dev_codegen(name),
    })

def _flags(manifest_dir, extra = [], host_code = False, target = None):
    if manifest_dir in ["crates/hirsel-llvm", "crates/hirsel-llvm-lazy"]:
        extra = extra + ["--check-cfg=cfg(hirsel_lazy_core)"]
    if manifest_dir == "crates/hirsel-llvm-lazy":
        extra = extra + ["--cfg=hirsel_lazy_core"]
    return ["-Zunstable-options", _env_set("CARGO_MANIFEST_DIR", _from_package(manifest_dir, manifest_dir))] + extra + workspace_profile_flags(host_code) + (_codegen_flags(target) if target else []) + runtime_profile_flags(manifest_dir.split("/")[-1].replace("-", "_"))

def _compile_env(package_name, crate, version, build_script):
    env = _cargo_env(package_name, crate, version)
    if package_name == "hirsel-llvm-lazy":
        env["OUT_DIR"] = "$(location //crates/hirsel-llvm-lazy/bridge:image)"
    if build_script != None:
        env = selects.apply(build_script, lambda script: env | ({"OUT_DIR": "$(location {})".format(script)} if script else {}))
    return env

# `-c hirsel.clippy=true` (tools/buck2/clippy.py) declares every Rust target's
# Clippy twin `<name>-clippy`: the same rule and attributes on a measured 1 CPU
# platform. Buck2 gives a target one execution platform, so Clippy
# on the target itself reserves its whole compile request. The twin shares its
# target's dependencies and sources: building its `[clippy.txt]` runs Clippy
# and nothing the build has not already run. Without the option the twins do
# not exist, so `buck2 build //...` never compiles them.
CLIPPY_TWINS = read_root_config("hirsel", "clippy", "false") == "true"

def _rust(rule, name, **attrs):
    if attrs.get("crate") == "hirsel_core_ids":
        attrs["features"] = attrs.get("features", []) + select({
            "root//tools/buck2:front_observer_enabled": ["front-observer"], "DEFAULT": [],
        })
    rule(name = name, **attrs)
    if CLIPPY_TWINS:
        twin = attrs | dict(exec_compatible_with = [budget_constraint(pool_budget_name(*CLIPPY_REQUEST))], visibility = ["PUBLIC"])
        twin.pop("doctests", None)
        twin.pop("labels", None)
        rule(name = name + "-clippy", **twin)

def hirsel_library(name, crate, crate_root, srcs, features, deps, manifest_dir, package_name, version, edition, build_script = None, staticlib = False, doctests = False):
    """A workspace library. With `doctests` it is also the test of its
    documentation examples, as `cargo test` runs them (label `doctest`)."""
    _rust(
        native.rust_library,
        name = name,
        crate = crate,
        crate_root = crate_root,
        srcs = srcs,
        features = features,
        deps = deps,
        edition = edition,
        env = _compile_env(package_name, crate, version, build_script),
        rustc_flags = _flags(manifest_dir, target = name),
        exec_compatible_with = _compile_constraints(name),
        doctests = doctests,
        doc_remote_execution = doctest_properties() if doctests else None,
        labels = ["doctest"] if doctests else [],
        visibility = ["PUBLIC"],
    )

def hirsel_binary(name, bin_name, crate, crate_root, srcs, features, deps, manifest_dir, package_name, version, edition, build_script = None):
    link_flags = select({
        "root//tools/buck2:profile_dist": [
            "-Clinker-features=-lld",
            "-Clink-arg=-fuse-ld=bfd",
            "-Clink-arg=-Wl,-z,nodynamic-undefined-weak",
        ],
        "DEFAULT": [],
    }) if name == "hirsel-core-driver-bin-hirsel" else []
    env = selects.apply(
        _compile_env(package_name, crate, version, build_script),
        lambda env: env | {"CARGO_BIN_NAME": bin_name},
    )
    _rust(
        native.rust_binary,
        name = name,
        crate = crate,
        crate_root = crate_root,
        srcs = srcs,
        features = features,
        deps = deps,
        edition = edition,
        env = env,
        rustc_flags = _flags(manifest_dir, EXECUTABLE_LINK_FLAGS + link_flags, target = name),
        exec_compatible_with = _compile_constraints(name),
        visibility = ["PUBLIC"],
    )

def native_tool_inputs():
    """The unpacked toolchains a native test reads, as `HIRSEL_TOOLCHAIN_<TARGET>`: the
    launcher sets the variables of `native-toolchain/environment.tsv` from them."""
    return {
        "HIRSEL_TOOLCHAIN_" + name.replace("-", "_").upper(): "root//tools/buck2/native-toolchain:" + name
        for name in ["arm", "qemu", "valgrind", "kit", "glibc_floor"]
    }

# An integration test crate (tests/) is test bodies over libraries that stay at
# the checked O3 with assertions. Its own code is glue: at O0 it compiled 2.4x
# faster (driver `it`, 131 s to 55 s on the pool) and its 60-shard sample ran no
# slower (585 s against 636 s), with the same results. Unit test crates compile
# the library itself and keep O3.
def _test_crate_flags(crate_root):
    if "/tests/" not in crate_root:
        return []
    return select({"root//tools/buck2:profile_checked": ["-Copt-level=0"], "DEFAULT": []})

def hirsel_test(name, crate, crate_root, srcs, features, deps, manifest_dir, package_name, version, edition, build_script = None, bin_exes = {}, data = [], files_env = {}, native_tools = False, harness_only = False):
    """A libtest harness (`rustc --test`) and the test that runs it.

    `bin_exes` are the package's binaries, as Cargo's CARGO_BIN_EXE_<name>.
    `data` are the files the test reads at run time.
    """
    harness = name if harness_only else name + "-harness"
    bin_exe_dir = "{}/{}".format(BIN_EXE_DIR, harness)
    exes = [_env_set("CARGO_BIN_EXE_" + bin, _from_package(manifest_dir, "{}/{}".format(bin_exe_dir, bin))) for bin in sorted(bin_exes)]
    _rust(
        native.rust_binary,
        name = harness,
        incoming_transition = "root//tools/buck2:test_tool_transition",
        crate = crate,
        crate_root = crate_root,
        srcs = srcs,
        features = features,
        deps = deps,
        edition = edition,
        env = _compile_env(package_name, crate, version, build_script),
        rustc_flags = _flags(manifest_dir, ["--test", _env_set("CARGO_TARGET_TMPDIR", _from_package(manifest_dir, TEST_TMPDIR))] + exes + EXECUTABLE_LINK_FLAGS, target = harness) + _test_crate_flags(crate_root),
        exec_compatible_with = _compile_constraints(harness),
        visibility = ["PUBLIC"],
    )
    if harness_only:
        return
    if package_name == "hirsel-core-rt":
        # Cross harness is opt-in, not a host test/tool or a shipping archive.
        # Same sources/deps as the ordinary harness, with real ARM cfg/std/link.
        native.rust_binary(
            name = "hirsel-core-rt-aarch64-harness",
            incoming_transition = "root//tools/buck2:aarch64_test_tool_transition",
            crate = crate,
            crate_root = crate_root,
            srcs = srcs,
            features = features,
            deps = deps,
            edition = edition,
            env = _compile_env(package_name, crate, version, build_script),
            rustc_flags = _flags(manifest_dir, ["--test"], target = "hirsel-core-rt-aarch64-harness"),
            exec_compatible_with = _compile_constraints("hirsel-core-rt-aarch64-harness"),
            labels = ["gate"],
            visibility = ["PUBLIC"],
        )
    common = dict(
        harness = ":" + harness,
        args = [],
        data = data,
        files_env = files_env,
        bin_exes = bin_exes,
        bin_exe_dir = bin_exe_dir,
        llvm = "root//tools/buck2/native-toolchain:llvm",
        llvm_libraries = "toolchains//:llvm-libraries",
        native_tools = native_tool_inputs() if native_tools else {},
        cwd = manifest_dir,
        test_binary = name,
        request = _request(name),
        visibility = ["PUBLIC"],
    )
    perf_cases = PERF_TEST_CASES.get(name, [])
    host_cases = OPENAT2_TEST_CASES.get(name, []) + perf_cases
    selection_env = {"HIRSEL_TEST_HOST_CASES": " ".join(host_cases)} if host_cases else {}
    _rust_test(
        name = name + "-shard-1" if host_cases else name,
        labels = ["local-execution"] if name in HOST_TEST_BINARIES else [],
        exec_compatible_with = _test_platform(name),
        pool = not (TESTS_ON_HOST or name in HOST_TEST_BINARIES),
        extra_env = selection_env,
        **common
    )
    isolated_common = {key: value for key, value in common.items() if key != "args"}
    if host_cases:
        _rust_test(
            name = name + "-inventory",
            args = ["--list"],
            labels = [],
            exec_compatible_with = _test_platform(name),
            pool = not TESTS_ON_HOST,
            **{key: value for key, value in isolated_common.items() if key != "test_binary"}
        )
    host_targets = []
    openat2_targets = []
    for index, case in enumerate(host_cases):
        requirement = "perf" if case in perf_cases else "openat2"
        target = "{}-{}-{}".format(name, requirement, index + 1)
        environment = {"HIRSEL_TEST_DEDICATED_CASE": case}
        if case in NATIVE_EXEC_TEST_CASES:
            environment["HIRSEL_TEST_NATIVE_EXEC"] = "1"
        host_targets.append(":" + target)
        if requirement == "openat2":
            openat2_targets.append(":" + target)
        _rust_test(
            name = target,
            args = common["args"] + ["--exact", case],
            labels = ["local-execution", "requires-" + requirement],
            exec_compatible_with = [budget_constraint("host")],
            pool = False,
            extra_env = environment,
            **isolated_common
        )
    if openat2_targets:
        _suite(name = name + "-openat2", tests = openat2_targets, labels = ["local-execution", "requires-openat2"])
    if host_cases:
        _suite(name = name, tests = host_targets + [":" + name + "-shard-1"])

def _rust_test_impl(ctx):
    binaries = dict(ctx.attrs.bin_exes)
    binaries.update(ctx.attrs.release_bin_exes)
    binaries.update(ctx.attrs.front_observer_bin_exes)
    exes = {bin: dep[DefaultInfo].default_outputs[0] for bin, dep in binaries.items()}
    inputs = ctx.attrs.data + [exe for _, exe in sorted(exes.items())] + [ctx.attrs._test_reporter, ctx.attrs._scratch_owner]
    tools = {name: dep[DefaultInfo].default_outputs[0] for name, dep in ctx.attrs.native_tools.items()}
    inputs += list(tools.values())
    env = {
        "HIRSEL_TEST_CWD": ctx.attrs.cwd,
        "HIRSEL_VALIDATE_STAGES": "1",
        "HIRSEL_TEST_TMPDIR": TEST_TMPDIR,
        "HIRSEL_SCRATCH_OWNER": cmd_args(ctx.attrs._scratch_owner),
        # The same effective request as the executor, including focus mode.
        "HIRSEL_TEST_CPUS": ctx.attrs.request[0],
        # A pool worker's action supervisor can attribute a test run's CPU to
        # these Bazel test variables. They are stable per
        # target and shard, as they are part of the action key.
        "TEST_TARGET": str(ctx.label.raw_target()),
        # Resident PSS sets the reservation. The allocation guard can also
        # include unused thread stacks (TEST_GUARD_LIMITS), without reserving
        # those virtual bytes on the pool.
        "HIRSEL_MEMORY_LIMIT": "{}K".format(test_memory_limit(ctx.attrs.test_binary, int(ctx.attrs.request[1]))),
    }
    if ctx.attrs.llvm:
        llvm = ctx.attrs.llvm[DefaultInfo].default_outputs[0]
        inputs.append(llvm)
        inputs += ctx.attrs.llvm_libraries[DefaultInfo].default_outputs
        env["LLVM_SYS_231_PREFIX"] = cmd_args(llvm)
    env.update(ctx.attrs.extra_env)
    env.update({name: cmd_args(tree) for name, tree in tools.items()})
    if tools:
        inputs.append(ctx.attrs._private_native_inputs)
        env["HIRSEL_PRIVATE_NATIVE_INPUTS"] = cmd_args(ctx.attrs._private_native_inputs)
        inputs.append(ctx.attrs._native_environment)
        env["HIRSEL_NATIVE_ENVIRONMENT"] = cmd_args(ctx.attrs._native_environment)
    if exes:
        env["HIRSEL_TEST_BIN_EXE_DIR"] = ctx.attrs.bin_exe_dir
        env["HIRSEL_TEST_BIN_EXES"] = cmd_args([cmd_args(bin, "=", exe, delimiter = "") for bin, exe in sorted(exes.items())], delimiter = " ")
    if ctx.attrs.test_binary:
        env["HIRSEL_TEST_BINARY"] = ctx.attrs.test_binary
    for name, file in ctx.attrs.files_env.items():
        inputs.append(file)
        env[name] = cmd_args(file)
    if ctx.attrs.pool:
        env["HIRSEL_TEST_POOL"] = "1"
        env["HIRSEL_TEST_REPORTER"] = cmd_args(ctx.attrs._test_reporter)
    command = cmd_args(ctx.attrs._launcher, ctx.attrs.harness[RunInfo], ctx.attrs.args, hidden = inputs)
    if ctx.attrs.fixture_output:
        fixture = ctx.actions.declare_output("fixture", dir = True)
        env["HIRSEL_REPLAY_FIXTURE_OUT"] = cmd_args(fixture.as_output())
        ctx.actions.run(command, env = env, category = "deadline_replay_fixture")
        return [DefaultInfo(default_output = fixture)]
    # Inputs can be built independently of running the test.
    sub_targets = {"inputs": [DefaultInfo(default_output = ctx.attrs.harness[DefaultInfo].default_outputs[0],
                                          other_outputs = inputs)]}
    return [
        DefaultInfo(default_output = ctx.attrs.harness[DefaultInfo].default_outputs[0], sub_targets = sub_targets),
        RunInfo(args = command),
        ExternalRunnerTestInfo(
            type = "rust",
            command = [command],
            env = env,
            labels = ctx.attrs.labels,
            run_from_project_root = True,
            use_project_relative_paths = True,
            # A pool run's passing verdict is cached by its action key (command,
            # environment and inputs), so a rerun on unchanged inputs does not
            # execute; a failure is never cached. The host platform has no cache.
            supports_test_execution_caching = ctx.attrs.pool,
            default_executor = test_executor(int(ctx.attrs.request[0]), int(ctx.attrs.request[1]), ctx.attrs.request[2]) if ctx.attrs.pool else None,
        ),
    ]

_rust_test = rule(
    impl = _rust_test_impl,
    attrs = {
        "args": attrs.list(attrs.string(), default = []),
        "bin_exe_dir": attrs.string(default = ""),
        "bin_exes": attrs.dict(attrs.string(), attrs.transition_dep(cfg = "root//tools/buck2:test_tool_transition"), default = {}),
        "front_observer_bin_exes": attrs.dict(attrs.string(), attrs.transition_dep(cfg = "root//tools/buck2:front_observer_tool_transition"), default = {}),
        "release_bin_exes": attrs.dict(attrs.string(), attrs.transition_dep(cfg = "root//tools/buck2:release_tool_transition"), default = {}),
        "cwd": attrs.string(),
        # Environment variables naming input files.
        "files_env": attrs.dict(attrs.string(), attrs.source(), default = {}),
        "extra_env": attrs.dict(attrs.string(), attrs.string(), default = {}),
        "fixture_output": attrs.bool(default = False),
        "data": attrs.list(attrs.source(), default = []),
        "harness": attrs.transition_dep(cfg = "root//tools/buck2:test_tool_transition", providers = [RunInfo]),
        "labels": attrs.list(attrs.string(), default = []),
        "llvm": attrs.option(attrs.dep(), default = None),
        "llvm_libraries": attrs.option(attrs.dep(), default = None),
        "native_tools": attrs.dict(attrs.string(), attrs.dep(), default = {}),
        "pool": attrs.bool(default = False),
        # A pool run's `[cpu, memory_kb, cpu_class]`, for its executor (platforms.bzl).
        "request": attrs.list(attrs.string(), default = []),
        "test_binary": attrs.string(default = ""),
        "_launcher": attrs.source(default = "root//tools/buck2:test_launcher.sh"),
        "_scratch_owner": attrs.source(default = "root//tools/buck2:scratch.py"),
        "_test_reporter": attrs.source(default = "root//tools/buck2:test_report.py"),
        "_private_native_inputs": attrs.source(default = "root//tools/buck2:private_native_inputs.py"),
        "_native_environment": attrs.source(default = "root//tools/buck2/native-toolchain:environment.tsv"),
    },
)

def _script_binary_impl(ctx):
    return [DefaultInfo(default_output = ctx.attrs.script), RunInfo(args = cmd_args(ctx.attrs.script))]

_script_binary = rule(impl = _script_binary_impl, attrs = {"script": attrs.source()})

def _test_suite_impl(ctx):
    inputs = []
    for dep in ctx.attrs.inputs:
        inputs.extend(dep[DefaultInfo].default_outputs)
        inputs.extend(dep[DefaultInfo].other_outputs)
    return [DefaultInfo(sub_targets = {"inputs": [DefaultInfo(other_outputs = inputs)]})]

# `buck2 test <suite>` runs the targets in its built-in `tests` attribute.
_test_suite = rule(impl = _test_suite_impl, attrs = {
    "labels": attrs.list(attrs.string(), default = []),
    "inputs": attrs.list(attrs.dep(), default = []),
})

def _suite(name, tests, **kwargs):
    _test_suite(name = name, tests = tests, inputs = [test + "[inputs]" for test in tests], **kwargs)

def hirsel_script_test(name, script, data, args = [], bin_exes = {}, front_observer_bin_exes = {}, production = False, files_env = {}, host = False, native_tools = False):
    """A script harness with the same hermetic inputs as native Rust tests.
    `files_env` names further input files as environment variables. `host`
    runs it on the invoking host, never the pool: for a tool built there.
    """
    binary = name + "-script"
    _script_binary(name = binary, script = script)
    _rust_test(
        name = name, harness = ":" + binary,
        args = args, data = data, cwd = ".",
        bin_exes = {} if production else bin_exes, release_bin_exes = bin_exes if production else {}, front_observer_bin_exes = front_observer_bin_exes,
        bin_exe_dir = BIN_EXE_DIR + "/" + name, files_env = files_env,
        llvm = "root//tools/buck2/native-toolchain:llvm",
        llvm_libraries = "toolchains//:llvm-libraries",
        native_tools = native_tool_inputs() if native_tools else {},
        labels = ["gate"],
        exec_compatible_with = [budget_constraint("host")] if host else _test_platform(name),
        pool = not (TESTS_ON_HOST or host),
        request = _request(name),
        visibility = ["PUBLIC"],
    )

PRODUCT_MEMORY_LIMIT = "2G"

# The platform host's root limit: its catalog store, the launcher session and each operator
# connection's reader and writer.
PLATFORM_HOST_LIMIT = 1073741824

def _core_image_impl(ctx):
    sources = ctx.actions.symlinked_dir("sources", {f.short_path: f for f in ctx.attrs.srcs})
    image = ctx.actions.declare_output("core-image", dir = True)
    ctx.actions.run(
        cmd_args(ctx.attrs.compiler[RunInfo], "image", "--libraries",
                 cmd_args(sources, format = "{}/core"), "--store", image.as_output(), "--json"),
        env = {"HIRSEL_MEMORY_LIMIT": PRODUCT_MEMORY_LIMIT, "HIRSEL_JOBS": "1"},
        category = "core_image",
    )
    return [DefaultInfo(default_output = image)]

_core_image = rule(
    impl = _core_image_impl,
    attrs = {
        "compiler": attrs.exec_dep(providers = [RunInfo]),
        "srcs": attrs.list(attrs.source()),
        # Producing a checked library is an explicit integration gate.
        "labels": attrs.list(attrs.string(), default = ["gate"]),
    },
)

def hirsel_core_image(name, **kwargs):
    compiler = name + "-compiler"
    native.configured_alias(name = compiler, actual = "//:hirsel-core-driver-bin-hirsel", platform = "root//tools/buck2:checked")
    _core_image(name = name, compiler = ":" + compiler,
                exec_compatible_with = [budget_constraint(pool_budget_name(*PRECHECK_REQUEST))],
                visibility = ["PUBLIC"], **kwargs)

def _store_package_impl(ctx):
    sources = ctx.actions.symlinked_dir("store-sources", {
        src.basename: src
        for src in ctx.attrs.sources
    })
    archive = ctx.actions.declare_output("store-1.hpkg")
    scratch = ctx.actions.declare_output("scratch", dir = True)
    script = ctx.actions.write("pack.sh", [
        "set -eu",
        'mkdir -p "$4/store"; cp "$2"/* "$4/store/"',
        '"$1" pack "$4/store" --package-store "$4/packages" --output "$3" > /dev/null',
    ])
    ctx.actions.run(
        cmd_args("/bin/sh", script, ctx.attrs.hirsel[DefaultInfo].default_outputs[0],
                 sources, archive.as_output(), scratch.as_output(),
                 hidden = ctx.attrs._llvm_libraries[DefaultInfo].default_outputs),
        env = {"HIRSEL_MEMORY_LIMIT": PRODUCT_MEMORY_LIMIT, "HIRSEL_JOBS": "1"},
        category = "store_package",
    )
    return [DefaultInfo(default_output = archive)]

hirsel_store_package = rule(
    impl = _store_package_impl,
    attrs = {
        "hirsel": attrs.dep(),
        "sources": attrs.list(attrs.source()),
        "_llvm_libraries": attrs.dep(default = "toolchains//:llvm-libraries"),
    },
)

def _toolchain_bundle_impl(ctx):
    # The self-contained installation the driver discovers beside itself: the
    # compiler as `hirsel` with RPATH $ORIGIN/lib, libLLVM and the libraries it
    # loads, ld.lld and the link kit, the runtime archive for each target, and the core and std
    # images, recorded in hirsel-core-toolchain.json, and the Store archive. The compiler copies
    # itself in; a request from the bundle loads nothing outside it. The bundled compiler then
    # builds the platform host as libexec/hirsel/platform_main (docs/spec/launch.md "Starting
    # the host") and its offline pin reader as libexec/hirsel/pin_reader: their grants name the state directory as /proc/self/cwd, the directory the
    # launcher starts the host in, and the driver the reader.
    bundle = ctx.actions.declare_output("hirsel-toolchain", dir = True)
    scratch = ctx.actions.declare_output("scratch", dir = True)
    runtimes = ctx.attrs.runtimes[DefaultInfo].sub_targets
    recording_runtimes = ctx.attrs.recording_runtimes[DefaultInfo].sub_targets
    prof_runtimes = ctx.attrs.prof_runtimes[DefaultInfo].sub_targets
    prof_recording_runtimes = ctx.attrs.prof_recording_runtimes[DefaultInfo].sub_targets
    script = ctx.actions.write("install.sh", [
        "set -eu",
        'mkdir -p "$2" "$3"; root="$(cd "$2" && pwd)"; export TMPDIR="$(cd "$3" && pwd)"',
        '"$1" core-product --install "$root" > /dev/null',
        '"$1" std-product --install "$root" --runtime "$4" --recording-runtime "$6" --prof-runtime "$9" --prof-recording-runtime "${11}" --target x86_64-unknown-linux-gnu --link-kit "$8" > /dev/null',
        '"$1" std-product --install "$root" --runtime "$5" --recording-runtime "$7" --prof-runtime "${10}" --prof-recording-runtime "${12}" --target aarch64-unknown-linux-gnu > /dev/null',
        'cp "${13}" "$root/core-packages/store-1.hpkg"',
        'mkdir -p "$TMPDIR/store" "$root/libexec/hirsel"; cp "${14}"/* "$TMPDIR/store/"',
        '"$root/hirsel" build "$TMPDIR/store" --entry store@1/platform_main/main --grant Clock --grant Random --grant Tasks'
        + ' --grant "Net=UnixRoot ctl /proc/self/cwd;UnixConnect ctl launcher.sock;UnixListen ctl operator.sock"'
        + ' --dir /proc/self/cwd/catalog --limit {} -o "$root/libexec/hirsel/platform_main" > /dev/null'.format(PLATFORM_HOST_LIMIT),
        '"$root/hirsel" build "$TMPDIR/store" --entry store@1/platform_main/read_pin --grant Clock --grant Stdout'
        + ' --dir /proc/self/cwd/catalog --limit {} -o "$root/libexec/hirsel/pin_reader" > /dev/null'.format(PLATFORM_HOST_LIMIT),
    ])
    store_sources = ctx.actions.symlinked_dir("store-sources", {
        src.basename: src
        for src in ctx.attrs.store_sources
    })
    ctx.actions.run(
        cmd_args("/bin/sh", script, ctx.attrs.hirsel[DefaultInfo].default_outputs[0],
                 bundle.as_output(), scratch.as_output(),
                 runtimes["x86_64"][DefaultInfo].default_outputs[0],
                 runtimes["aarch64"][DefaultInfo].default_outputs[0],
                 recording_runtimes["x86_64"][DefaultInfo].default_outputs[0],
                 recording_runtimes["aarch64"][DefaultInfo].default_outputs[0],
                 ctx.attrs._link_kit[DefaultInfo].default_outputs[0],
                 prof_runtimes["x86_64"][DefaultInfo].default_outputs[0],
                 prof_runtimes["aarch64"][DefaultInfo].default_outputs[0],
                 prof_recording_runtimes["x86_64"][DefaultInfo].default_outputs[0],
                 prof_recording_runtimes["aarch64"][DefaultInfo].default_outputs[0],
                 ctx.attrs.store_package[DefaultInfo].default_outputs[0],
                 store_sources,
                 hidden = ctx.attrs._llvm_libraries[DefaultInfo].default_outputs),
        env = {"HIRSEL_MEMORY_LIMIT": PRODUCT_MEMORY_LIMIT, "HIRSEL_JOBS": "1"},
        category = "toolchain_bundle",
    )
    return [
        DefaultInfo(default_output = bundle),
        RunInfo(args = cmd_args(bundle, format = "{}/hirsel")),
    ]

hirsel_toolchain_bundle = rule(
    impl = _toolchain_bundle_impl,
    attrs = {
        "hirsel": attrs.dep(),
        "runtimes": attrs.dep(),
        "recording_runtimes": attrs.dep(),
        "prof_runtimes": attrs.dep(),
        "prof_recording_runtimes": attrs.dep(),
        "store_package": attrs.dep(),
        "store_sources": attrs.list(attrs.source()),
        "_link_kit": attrs.dep(default = "root//tools/buck2/native-toolchain:kit"),
        "_llvm_libraries": attrs.dep(default = "toolchains//:llvm-libraries"),
    },
)

def _workspace_build_script_run_impl(ctx):
    # Lay the declared workspace files out at their repository paths, so the
    # script finds them from CARGO_MANIFEST_DIR as it does under Cargo.
    inputs = {f.short_path: f for f in ctx.attrs.inputs}
    inputs.update(ctx.attrs.input_paths)
    tree = ctx.actions.symlinked_dir("workspace", inputs)
    out_dir = ctx.actions.declare_output("OUT_DIR", dir = True)
    script = ctx.actions.write(
        "run.sh",
        [
            "set -eu",
            'tree="$(cd "$1" && pwd)"; script="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"; mkdir -p "$3"; out="$(cd "$3" && pwd)"',
            'cd "$tree/{}"'.format(ctx.attrs.manifest_dir),
            'CARGO_MANIFEST_DIR="$tree/{}" OUT_DIR="$out" "$script" > /dev/null'.format(ctx.attrs.manifest_dir),
        ],
    )
    ctx.actions.run(
        cmd_args("/bin/sh", script, tree, ctx.attrs.script[RunInfo], out_dir.as_output()),
        env = {
            "CARGO_PKG_NAME": ctx.attrs.package_name,
            "CARGO_PKG_VERSION": ctx.attrs.version,
            "TARGET": "x86_64-unknown-linux-gnu",
            "HOST": "x86_64-unknown-linux-gnu",
            "PROFILE": "debug",
        } | {"CARGO_FEATURE_" + f.upper().replace("-", "_"): "1" for f in ctx.attrs.features},
        category = "build_script_run",
        allow_cache_upload = True,
    )
    return [DefaultInfo(default_output = out_dir)]

_workspace_build_script_run = rule(
    impl = _workspace_build_script_run_impl,
    attrs = {
        "features": attrs.list(attrs.string(), default = []),
        "inputs": attrs.list(attrs.source()),
        "input_paths": attrs.dict(attrs.string(), attrs.source(), default = {}),
        "manifest_dir": attrs.string(),
        "package_name": attrs.string(),
        "script": attrs.exec_dep(providers = [RunInfo]),
        "version": attrs.string(),
    },
)

def hirsel_build_script(name, crate_root, srcs, deps, inputs, features, manifest_dir, package_name, version, edition, input_paths = {}, compile_features = None):
    """A Cargo build script: compiled once, then run over its declared inputs.

    Its output directory is the library's OUT_DIR. Cargo rerun directives are
    discarded: they contain sandbox paths and directory traversal order, and
    Buck2 already tracks the declared inputs. Any other directives a script
    needs must be translated here.
    """
    _rust(
        native.rust_binary,
        name = name + "-bin",
        incoming_transition = "root//tools/buck2:host_transition",
        crate = "build_script_build",
        features = features if compile_features == None else compile_features,
        crate_root = crate_root,
        srcs = srcs,
        deps = deps,
        edition = edition,
        env = _cargo_env(package_name, "build_script_build", version),
        rustc_flags = _flags(manifest_dir, host_code = True, target = name + "-bin"),
        exec_compatible_with = _compile_constraints(name + "-bin"),
    )
    _workspace_build_script_run(
        name = name,
        script = ":" + name + "-bin",
        inputs = inputs + [manifest_dir + "/Cargo.toml"],
        input_paths = input_paths,
        features = features,
        manifest_dir = manifest_dir,
        package_name = package_name,
        version = version,
    )

RUNTIME_TARGETS = {
    "aarch64-unknown-linux-gnu": "root//tools/buck2:runtime-aarch64",
    "x86_64-unknown-linux-gnu": "root//tools/buck2:runtime-x86_64",
}

def _pic_staticlib_archive(library):
    # The prelude's staticlib subtarget requests a non-PIC archive. Its native
    # static_pic link provider already names the bundled PIC staticlib, with
    # the same profile, assertions, panic policy and dependency feature graph.
    infos = library[MergedLinkInfo]._infos[LinkStrategy("static_pic")]
    for info in infos.traverse():
        for linkable in info.default.linkables:
            if isinstance(linkable, ArchiveLinkable):
                return linkable.archive.artifact
    fail("Rust native static_pic provider has no bundled archive")

def _runtime_without_debug(ctx, library, path):
    stripped = ctx.actions.declare_output(path)
    command = cmd_args(
        "python3", ctx.attrs._parts, "--llvm", ctx.attrs._llvm[DefaultInfo].default_outputs[0],
        "--archive", _pic_staticlib_archive(library), "--stripped", stripped.as_output(),
    )
    # Buck separates native link dependencies from rustc's bundled staticlib.
    # Include their PIC objects so this archive links outside Buck, as Cargo's
    # staticlib does.
    archives = {}
    objects = {}
    thin = {}
    for native in dfs_dedupe_by_label(library[RustLinkInfo].native_link_deps):
        infos = native._infos.get(LinkStrategy("static_pic"))
        if infos == None:
            continue
        for info in infos.traverse():
            for linkable in info.default.linkables:
                if isinstance(linkable, ArchiveLinkable):
                    archives[linkable.archive.artifact] = None
                    for obj in linkable.archive.external_objects:
                        thin[obj] = None
                elif isinstance(linkable, ObjectsLinkable):
                    for obj in linkable.objects or []:
                        objects[obj] = None
                else:
                    fail("runtime staticlib has an unbundled native dependency: {}".format(linkable))
    for archive in archives:
        command.add("--native-archive", archive)
    for obj in objects:
        command.add("--native-object", obj)
    command.add(cmd_args(hidden = list(thin)))
    ctx.actions.run(command, category = "core_runtime_parts", identifier = path.replace("/", "-"))
    return stripped

def _pic_staticlib_impl(ctx):
    archive = _runtime_without_debug(ctx, ctx.attrs.library, "libhirsel_core_rt.a")
    return [DefaultInfo(default_output = archive)]

_pic_staticlib = rule(
    impl = _pic_staticlib_impl,
    attrs = {
        "library": attrs.dep(providers = [MergedLinkInfo, RustLinkInfo]),
        "_parts": attrs.source(default = "//tools/buck2:runtime_parts.py"),
        "_llvm": attrs.dep(default = "toolchains//:clang"),
    },
)

def hirsel_pic_staticlib(name, library):
    _pic_staticlib(name = name, library = library, visibility = ["PUBLIC"])

def hirsel_core_runtime_archives(name, library, archive_name):
    """The Core runtime, in its own archive namespace on both targets.

    Use the library's Cargo-resolved dependencies.
    """
    archives = {}
    for triple, platform in RUNTIME_TARGETS.items():
        configured = name + "-" + triple.split("-")[0]
        native.configured_alias(name = configured, actual = library, platform = platform)
        archives[triple] = ":" + configured
    _core_runtime_archives(name = name, archives = archives, archive_name = archive_name,
                           visibility = ["PUBLIC"])

def _core_runtime_archives_impl(ctx):
    entries = {}
    views = {}
    for triple, dep in ctx.attrs.archives.items():
        archive = _runtime_without_debug(ctx, dep, triple + "/" + ctx.attrs.archive_name)
        entries[triple + "/" + ctx.attrs.archive_name] = archive
        views[triple.split("-")[0]] = [DefaultInfo(default_output = archive)]
    return [DefaultInfo(default_output = ctx.actions.symlinked_dir(ctx.label.name, entries), sub_targets = views)]

_core_runtime_archives = rule(
    impl = _core_runtime_archives_impl,
    attrs = {
        "archives": attrs.dict(attrs.string(), attrs.dep()),
        "archive_name": attrs.string(),
        "_parts": attrs.source(default = "//tools/buck2:runtime_parts.py"),
        "_llvm": attrs.dep(default = "toolchains//:clang"),
    },
)

def hirsel_core_driver_native_test(name, data, case_filter = "native::tests::"):
    files_env = {
        "HIRSEL_CORE_RT_ARCHIVE": "//:core-driver-runtime-pic",
        "HIRSEL_CORE_RT_RECORD_ARCHIVE": "//:core-driver-record-runtime-pic",
        # A runtime that writes the logical charge report.
        "HIRSEL_CORE_RT_OBSERVED_ARCHIVE": "//:core-driver-observed-runtime-pic",
        "HIRSEL_CORE_RT_PROF_ARCHIVE": "//:hirsel-core-rt-prof-archives[x86_64]",
        "HIRSEL_CORE_RT_PROF_RECORD_ARCHIVE": "//:hirsel-core-rt-prof-record-archives[x86_64]",
    }
    if case_filter == "native::tests::":
        files_env["HIRSEL_CORE_RT_OBSERVED_RECORD_ARCHIVE"] = "//:hirsel-core-rt-observed-record-archives[x86_64]"
        files_env["HIRSEL_CORE_RT_SCRIPTED_ARCHIVE"] = "//:core-scripted-runtime-pic"
        files_env["HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE"] = "//:core-scripted-record-runtime-pic"
        files_env["HIRSEL_TEST_TAIL_CONTEXT"] = "//crates/hirsel-llvm-lazy/bridge:tail-context-test"
    cases = OPENAT2_TEST_CASES["hirsel-core-driver-unittest"] if case_filter == "native::tests::" else []
    common = dict(
        harness = "//:hirsel-core-driver-unittest-harness",
        data = data,
        files_env = files_env,
        cwd = "crates/hirsel-core-driver",
        test_binary = "hirsel-core-driver-native",
        llvm = "root//tools/buck2/native-toolchain:llvm",
        llvm_libraries = "toolchains//:llvm-libraries",
        request = _request("hirsel-core-driver-unittest"),
        visibility = ["PUBLIC"],
    )
    _rust_test(
        name = name + "-pool",
        args = [case_filter, "--ignored"] + [arg for case in cases for arg in ["--skip", case]],
        exec_compatible_with = _test_platform("hirsel-core-driver-unittest"),
        pool = not TESTS_ON_HOST,
        **common
    )
    host_targets = []
    for index, case in enumerate(cases):
        target = "{}-openat2-{}".format(name, index + 1)
        environment = {"HIRSEL_TEST_DEDICATED_CASE": case}
        if case in NATIVE_EXEC_TEST_CASES:
            environment["HIRSEL_TEST_NATIVE_EXEC"] = "1"
        host_targets.append(":" + target)
        _rust_test(
            name = target,
            args = ["--include-ignored", "--exact", case],
            labels = ["local-execution", "requires-openat2"],
            exec_compatible_with = [budget_constraint("host")],
            pool = False,
            extra_env = environment,
            **common
        )
    if cases:
        _suite(name = name + "-openat2", tests = host_targets, labels = ["local-execution", "requires-openat2"])
    _suite(name = name, tests = [":" + name + "-pool"] + host_targets)

def hirsel_core_llvm_native_test(name, data, selection = None, runtime = "//:core-native-runtime-fixture-pic", binary = "hirsel-llvm-unittest", cwd = "crates/hirsel-llvm", extra_env = {}):
    """CoreIR executable agreement uses the core runtime, with no old std products."""
    _rust_test(
        name = name,
        harness = "//:" + binary + "-harness",
        data = data + [cwd + "/Cargo.toml"],
        files_env = {"HIRSEL_CORE_RT_ARCHIVE": runtime},
        args = selection if selection != None else ["core::", "context::probe_tests::"],
        extra_env = extra_env,
        native_tools = native_tool_inputs(),
        cwd = cwd,
        test_binary = binary,
        llvm = "root//tools/buck2/native-toolchain:llvm",
        llvm_libraries = "toolchains//:llvm-libraries",
        request = _request(binary),
        exec_compatible_with = _test_platform(binary),
        pool = not TESTS_ON_HOST,
        visibility = ["PUBLIC"],
    )
