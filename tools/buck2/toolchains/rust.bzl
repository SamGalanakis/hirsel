load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")

def _sysroot_files(ctx):
    # A target whose std is not in every sysroot (target_std) joins it here.
    files = dict(ctx.attrs.srcs)
    files.update(ctx.attrs.target_std)
    return files

def _trace_std(ctx):
    tree = ctx.actions.copied_dir("rust", _sysroot_files(ctx))
    std = ctx.actions.declare_output("libstd-hirsel-trace.rlib")
    ctx.actions.run(cmd_args(
        "python3", ctx.attrs.script, tree, ctx.attrs.std_sources,
        ctx.attrs.panic_image, ctx.attrs.target_triple, std.as_output(),
    ), category = "rust_trace_only_std")
    return [DefaultInfo(default_output = std)]

trace_std = rule(
    impl = _trace_std,
    attrs = {
        "srcs": attrs.dict(attrs.string(), attrs.source()),
        "std_sources": attrs.source(allow_directory = True),
        "target_triple": attrs.string(),
        "target_std": attrs.dict(attrs.string(), attrs.source(), default = {}),
        "script": attrs.source(),
        "panic_image": attrs.source(),
    },
)

def _rust(ctx):
    tree = ctx.actions.copied_dir("rust", ctx.attrs.srcs)
    sysroot = tree
    if ctx.attrs.target_std:
        sysroot = ctx.actions.copied_dir("target-rust", _sysroot_files(ctx))
    if ctx.attrs.runtime_std != None:
        target_lib = "lib/rustlib/{}/lib/".format(ctx.attrs.target_triple)
        std = ctx.attrs.runtime_std[DefaultInfo].default_outputs[0]
        files = {name: source for name, source in _sysroot_files(ctx).items() if not name.startswith(target_lib + "libstd-")}
        files[target_lib + "libstd-hirsel-trace.rlib"] = std
        sysroot = ctx.actions.copied_dir("runtime-rust", files)
    rustc_env = {"PATH": "/usr/bin:/bin"}
    if ctx.attrs.deployment_target != None:
        rustc_env["MACOSX_DEPLOYMENT_TARGET"] = ctx.attrs.deployment_target
    return [
        DefaultInfo(),
        RustToolchainInfo(
            compiler = RunInfo(args = cmd_args(tree.project("bin/rustc"), hidden = tree)),
            rustdoc = RunInfo(args = cmd_args(tree.project("bin/rustdoc"), hidden = tree)),
            clippy_driver = RunInfo(args = cmd_args(tree.project("bin/clippy-driver"), hidden = tree)),
            # Prelude actions name the declared sysroot instead of discovering it
            # from the executable's location.
            sysroot_path = sysroot,
            default_edition = "2024",
            panic_runtime = PanicRuntime("unwind"),
            # Cargo's dev profile for workspace members ([profile.dev] in Cargo.toml):
            # debug assertions and overflow checks on, line tables only. Profile
            # flags that differ per package are added by tools/buck2/defs.bzl.
            rustc_flags = [
                "-Cdebuginfo=line-tables-only",
                "-Cdebug-assertions=yes",
                "-Coverflow-checks=yes",
                "-Cembed-bitcode=no",
                "-Cstrip=none",
            ],
            extra_rustc_flags = [],
            # The prelude gates -Zno-codegen metadata pipelining on this field;
            # stable rustc runs it under RUSTC_BOOTSTRAP.
            nightly_features = True,
            rustc_env = rustc_env,
            rustc_target_triple = ctx.attrs.target_triple,
            doctests = False,
        ),
    ]

rust_toolchain = rule(
    impl = _rust,
    attrs = {
        "srcs": attrs.dict(attrs.string(), attrs.source()),
        "runtime_std": attrs.option(attrs.dep(), default = None),
        "target_triple": attrs.string(),
        "target_std": attrs.dict(attrs.string(), attrs.source(), default = {}),
        "deployment_target": attrs.option(attrs.string(), default = None),
    },
    is_toolchain_rule = True,
)
