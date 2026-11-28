"""Build the locked reference servers and the source-based Hirsel fixtures."""

import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/toolchain-dist'))
import hcfg

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
MICRO = ROOT / "bench/micro/http"
PINNED_CC = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"
CAPPED = ROOT / "tools/capped"
COMPILER = "//:hirsel-core-driver-bin-hirsel"
RUNTIME = "//:core-driver-runtime-pic"


def run(command, *, cwd=ROOT, capture=False, extra_env=None):
    env = {**os.environ, "HIRSEL_MEMORY_LIMIT": "2G", **(extra_env or {})}
    return subprocess.run(command, cwd=cwd, env=env, text=True, check=True,
                          stdout=subprocess.PIPE if capture else sys.stderr).stdout


def buck(profile, targets):
    output = run([ROOT / ".buck2/bin/buck2", "build",
                  "--target-platforms", f"//tools/buck2:{profile}", "--show-output", *targets],
                 capture=True)
    artifacts = dict(line.removeprefix("root").split(maxsplit=1)
                     for line in output.splitlines() if line.startswith("root//:"))
    return {label: ROOT / artifacts[label] for label in targets}


def cargo(workspace, binaries, frame_pointers=False):
    env = {"RUSTFLAGS": "-Cforce-frame-pointers=yes"} if frame_pointers else {}
    output = run([CAPPED, "cargo", "build", "--manifest-path", workspace / "Cargo.toml",
                  "--workspace", "--all-targets", "--locked", "--release", "--message-format=json"],
                 capture=True, extra_env=env)
    result = {}
    for line in output.splitlines():
        row = json.loads(line)
        if row.get("executable") and not row["profile"]["test"]:
            name = row["target"]["name"]
            if name in binaries:
                result[name] = Path(row["executable"])
    if set(result) != set(binaries):
        raise RuntimeError(f"missing Rust artifacts: {set(binaries).difference(result)}")
    return result


def manifest_for(fixture, source, service=False):
    sys.path.insert(0, str(ROOT / "core/tools"))
    from std_native import Contract, manifest

    base = manifest(ROOT, Contract(fixture, 0, "", "", True, test_of="http"), source)
    if not service:
        return base
    core, std = (item["package"] for item in base["packages"])
    store = {"name": "store", "version": "1"}
    store_root = ROOT / "core/programs/store"
    store_sources = hcfg.package(store_root)["sources"]
    base["packages"].extend([
        {"package": store, "dependencies": [core, std],
         "sources": [{"file": str(store_root / file), "modules": [module], "signatures": False}
                     for file, module in store_sources.items()]},
        {"package": {"name": "bench", "version": "1"}, "dependencies": [core, std, store],
         "sources": [{"file": str(fixture), "modules": ["service"], "signatures": False}]},
    ])
    return base


def native(tools, paths, output, entry, document, service=False):
    manifest = paths / f"{output.name}-inputs.json"
    manifest.write_text(json.dumps(document))
    command = [CAPPED, tools[COMPILER], "build", "--release", "--backend", "llvm",
               "--source-core", "--inputs", manifest, "--entry", entry,
               "--runtime", tools[RUNTIME], "--cc", PINNED_CC,
               "--store", paths / "native-build", "-o", output, "--grant", "Clock", "--grant", "Stdout"]
    private_homes = {"XDG_DATA_HOME": str(paths / f"{output.name}-data"),
                     "XDG_CACHE_HOME": str(paths / f"{output.name}-cache")}
    if service:
        directory = paths / "hirsel-db"
        directory.mkdir()
        try:
            command += ["--dir", directory, "--limit", "1073741824",
                        "--grant", "Tasks", "--grant", "Random", "--grant", "Stderr",
                        "--grant", "Net=Connect 127.0.0.1/32 1-65535;Listen 127.0.0.1/32 0-65535"]
            run(command, extra_env=private_homes)
        finally:
            directory.rmdir()
    else:
        # Keep the former 64 MiB default for the B1 server.
        run([*command, "--limit", str(64 << 20), "--debug-lines", "on", "--grant", "Stderr",
             "--grant", "Net=Listen 127.0.0.1/32 0-65535"],
            extra_env=private_homes)
    return output


def build_b1(paths):
    fixtures = MICRO / "hirsel.hirsel"
    source = paths / "http.hirsel"
    source.write_text((ROOT / "core/std/http.hirsel").read_text() + "\n" + fixtures.read_text())
    tools = buck("profiling", (COMPILER, RUNTIME))
    compare = cargo(MICRO, ("http-b1-tokio", "http-b1-hyper"), frame_pointers=True)
    binaries = {key.removeprefix("http-b1-"): path for key, path in compare.items()}
    binaries["hirsel"] = native(tools, paths, paths / "hirsel-b1", "std@1/http/b1_main",
                                manifest_for(fixtures, source))
    return binaries, source


def build_services(paths):
    tools = buck("release", (COMPILER, RUNTIME))
    axum = cargo(HERE / "rust", ("http-service-axum",))["http-service-axum"]
    (paths / "axum").symlink_to(axum)
    for package, binary in (("server", "go"), ("load", "load")):
        run([CAPPED, "go", "build", "-mod=readonly", "-o", paths / binary, f"./{package}"],
            cwd=HERE / "go")
    run(["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared",
         "-o", paths / "allocation_count.so", HERE / "allocation_count.c"])
    fixture = HERE / "service.hirsel"
    native(tools, paths, paths / "hirsel", "bench@1/service/main",
           manifest_for(fixture, ROOT / "core/std/http.hirsel", service=True),
           service=True)
