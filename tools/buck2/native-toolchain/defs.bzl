"""Pinned downloads become relocatable tool directories.

Unpacking and the link kit run on the pool like any compile (the default
platform, platforms.bzl), so the remote action cache shares their outputs
between checkouts instead of every worktree redoing them on this host. They
use python3, libzstd and readelf from the worker image, plus the pinned LLVM
tree. Downloads are deferred: with a SHA-256 the daemon fetches an archive
only when the pool's CAS lacks it, and not at all on an action-cache hit
(buck2 app/buck2_action_impl/src/actions/impls/download_file.rs `declare_http`,
app/buck2_execute/src/re/uploader.rs `find_missing`)."""
load("//tools/buck2:platforms.bzl", "budget_constraint", "test_budget_name", "test_executor", "test_request")
load("@toolchains//:apple.bzl", "APPLE_TARGETS")

LIBRARIES = ["libSystem", "CoreFoundation", "Security"]
DARWIN_SYMBOLS = {library: "root//:darwin-{}-symbols".format(library.lower()) for library in LIBRARIES}
IOS_SYMBOLS = {library: "root//:ios-{}-symbols".format(library.lower()) for library in LIBRARIES}

DEB_UNPACK = "//tools/buck2/native-toolchain:deb_unpack.py"

def _unpack(ctx):
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    ctx.actions.run(
        cmd_args("python3", ctx.attrs._unpack, ctx.attrs.kind, output.as_output(),
                 [cmd_args("--linker=", ctx.attrs.linker[DefaultInfo].default_outputs[0], delimiter = "")] if ctx.attrs.linker else [],
                 [dep[DefaultInfo].default_outputs[0] for dep in ctx.attrs.archives],
                 hidden = [ctx.attrs._deb_unpack]),
        category = "native_toolchain_unpack",
    )
    return [DefaultInfo(default_output = output)]

unpack_toolchain = rule(impl = _unpack, attrs = {
    "archives": attrs.list(attrs.dep()),
    "kind": attrs.string(),
    # kind llvm: the executable placed as bin/ld.lld instead of the deb's.
    "linker": attrs.option(attrs.dep(), default = None),
    "_unpack": attrs.source(default = "//tools/buck2/native-toolchain:unpack-source"),
    "_deb_unpack": attrs.source(default = DEB_UNPACK),
})

def _link_kit(ctx):
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    pins = ctx.actions.write_json("pins.json", ctx.attrs.pins)
    apple_targets = ctx.actions.write_json("apple-targets.json", APPLE_TARGETS)
    source = lambda name: ctx.attrs.sources[name][DefaultInfo].default_outputs[0]
    command = cmd_args(
        "python3", ctx.attrs._script,
        "--llvm", ctx.attrs.llvm[DefaultInfo].default_outputs[0],
        "--abilists", source("abilists"),
        "--arm-sysroot", cmd_args(ctx.attrs.arm[DefaultInfo].default_outputs[0], "/aarch64-none-linux-gnu/libc", delimiter = ""),
        "--libunwind", source("libunwind"),
        "--compiler-rt", source("compiler-rt"),
        "--zig-libsystem", source("libSystem-tbd"),
        "--apple-targets", apple_targets,
        [cmd_args("--darwin-symbols", cmd_args(library, "=", path, delimiter = "")) for library, path in sorted(ctx.attrs._darwin_symbols.items())],
        [cmd_args("--ios-symbols", cmd_args(library, "=", path, delimiter = "")) for library, path in sorted(ctx.attrs._ios_symbols.items())],
        "--pins", pins,
        "--output", output.as_output(),
        hidden = [ctx.attrs._deb_unpack],
    )
    for name in sorted(ctx.attrs.sources):
        if ":" in name:
            command.add("--deb", cmd_args(name, "=", source(name), delimiter = ""))
    ctx.actions.run(command, category = "link_kit")
    return [DefaultInfo(default_output = output)]

# The per-target link kit (link_kit.py): built once from pinned sources and
# hashed into every executable's build identity.
link_kit = rule(impl = _link_kit, attrs = {
    "llvm": attrs.dep(),
    "arm": attrs.dep(),
    "sources": attrs.dict(attrs.string(), attrs.dep()),
    "pins": attrs.dict(attrs.string(), attrs.dict(attrs.string(), attrs.any())),
    "_script": attrs.source(default = "//tools/buck2/native-toolchain:link-kit-source"),
    "_deb_unpack": attrs.source(default = DEB_UNPACK),
    "_darwin_symbols": attrs.dict(attrs.string(), attrs.source(), default = DARWIN_SYMBOLS),
    "_ios_symbols": attrs.dict(attrs.string(), attrs.source(), default = IOS_SYMBOLS),
})

def _proof(ctx):
    trees = [dep[DefaultInfo].default_outputs[0] for dep in ctx.attrs.tools]
    ios_targets = ctx.actions.write_json("ios-targets.json", [row for row in APPLE_TARGETS.values() if row["family"] == "ios"])
    command = cmd_args(
        ctx.attrs.interpreter, ctx.attrs.script, trees, APPLE_TARGETS["macos"]["triple"],
        ctx.attrs._ios_proof, ctx.attrs._link_kit_source, ios_targets,
        ctx.attrs._ios_runtimes[DefaultInfo].default_outputs[0],
        ctx.attrs._macos_libsystem, [ctx.attrs._ios_symbols[library] for library in LIBRARIES],
    )
    return [DefaultInfo(), ExternalRunnerTestInfo(
        type = "custom", command = [command], env = {"HIRSEL_PLATFORM_CAPTURE": "cold"}, labels = ["gate"],
        run_from_project_root = True, use_project_relative_paths = True,
        supports_test_execution_caching = True,
        default_executor = test_executor(int(ctx.attrs.request[0]), int(ctx.attrs.request[1]), ctx.attrs.request[2]),
    )]

_toolchain_proof = rule(impl = _proof, attrs = {
    "tools": attrs.list(attrs.dep()), "script": attrs.source(),
    "request": attrs.list(attrs.string()),
    "interpreter": attrs.string(default = "bash"),
    "_ios_proof": attrs.source(default = "//tools/buck2/native-toolchain:ios_proof.py"),
    "_link_kit_source": attrs.source(default = "//tools/buck2/native-toolchain:link-kit-source"),
    "_ios_runtimes": attrs.dep(default = "root//:hirsel-core-rt-ios-embed-archives"),
    "_macos_libsystem": attrs.source(default = DARWIN_SYMBOLS["libSystem"]),
    "_ios_symbols": attrs.dict(attrs.string(), attrs.source(), default = IOS_SYMBOLS),
})

def toolchain_proof(name, tools, script, interpreter = "bash"):
    _toolchain_proof(name = name, tools = tools, script = script, interpreter = interpreter,
        request = [str(value) for value in test_request("native-toolchain-proof")],
        exec_compatible_with = [budget_constraint(test_budget_name("native-toolchain-proof"))],
        visibility = ["PUBLIC"])

