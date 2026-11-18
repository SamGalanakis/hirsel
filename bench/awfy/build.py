#!/usr/bin/env python3
"""Build the AWFY binaries with the LangArena release profiles."""
import argparse
import ast
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core/tools"))
from langarena import inputs


def llvm_path():
    return ROOT / "tools/buck2/toolchains/llvm-files"


def capped_env(out):
    return {**os.environ, "HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "1",
            "XDG_DATA_HOME": str(out / "data"), "XDG_CACHE_HOME": str(out / "cache")}


def fixture(upstream):
    source = (upstream / "benchmarks/JavaScript/json.js").read_text()
    prefix = "const RAP_BENCHMARK_MINIFIED = "
    literal, = (line[len(prefix):-1] for line in source.splitlines() if line.startswith(prefix))
    text = ast.literal_eval(literal)
    return json.dumps(text, ensure_ascii=False)


def hirsel(args):
    artifacts = json.loads(args.artifacts.read_text())
    compiler, runtime = (artifacts[key] for key in ("compiler", "runtime"))
    spec = inputs(ROOT, "harness")
    rap = args.out / "rap.hirsel"
    rap.write_text(f"pub fn document() -> String {{ {fixture(args.upstream)} }}\n")
    spec["packages"][-1].update(package={"name": "awfy", "version": "1"}, sources=[
        {"file": str(source), "modules": [source.stem], "signatures": False}
        for source in [*sorted((HERE / "hirsel").glob("*.hirsel")), rap]
    ])
    out = args.out
    manifest = out / "hirsel-inputs.json"
    manifest.write_text(json.dumps(spec))
    for module in args.hirsel_entries:
        subprocess.run([
            str(ROOT / "tools/capped"), compiler, "build", "--inputs", str(manifest),
            "--entry", f"awfy@1/{module}/main", "--store", str(out / "store"),
            "--backend", "llvm", "--release", "--debug-lines", "on", "--runtime", runtime,
            "--cc", str(llvm_path() / "bin/clang"),
            "--grant", "Stdout", "-o", str(out / f"hirsel-{module}"),
        ], env={**capped_env(out), "LD_LIBRARY_PATH": str(llvm_path() / "lib")}, check=True)


def goose(args):
    source = args.out / "goose-src"
    shutil.copytree(HERE / "goose", source, dirs_exist_ok=True)
    (source / "rap.goose").write_text(
        f"namespace rap;\nfn document() -> const u8[:] {{ return {fixture(args.upstream)}; }}\n")
    command = [ROOT / "tools/capped"]
    env = capped_env(args.out)
    for module in args.goose_entries:
        generated = args.out / f"goose-{module}.c"
        subprocess.run([*command, args.goose, "-O2", "--standalone", "-o", generated,
                        source / (module + ".goose")], env=env, check=True)
        subprocess.run([*command, "clang", "-O2", "-w", "-ffp-contract=off", "-gline-tables-only",
                        generated, "-o", args.out / f"goose-{module}", "-lm", "-pthread", "-ldl"],
                       env=env, check=True)


def rust(args):
    for module, source in (("hello", HERE / "hello.rs"), ("harness", args.rust / "src/main.rs")):
        subprocess.run([ROOT / "tools/capped", "rustc", "--edition=2021", "-C", "opt-level=3", "-C", "debuginfo=1",
                        source, "-o", args.out / f"rust-{module}"], env=capped_env(args.out), check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--upstream", type=Path, required=True, help="pinned AWFY checkout")
    parser.add_argument("--artifacts", type=Path, help="JSON from core/tools/measurement_artifacts.py")
    parser.add_argument("--hirsel-entries", nargs="+", default=["hello", "harness", "pools"])
    parser.add_argument("--goose-entries", choices=["hello", "harness", "pools"], nargs="+",
                        default=["hello", "harness", "pools"])
    parser.add_argument("--goose", type=Path, help="Goose release compiler")
    parser.add_argument("--rust", type=Path, help="Resid's bench/v2/t1-awfy/rust directory")
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.artifacts:
        hirsel(args)
    if args.goose:
        goose(args)
    if args.rust:
        rust(args)


if __name__ == "__main__":
    main()
