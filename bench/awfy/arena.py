#!/usr/bin/env python3
"""Build the LangArena Hirsel groups and obtain one-iteration Rust checksums."""
import argparse
import json
from pathlib import Path
import re
import subprocess

from build import ROOT, capped_env, inputs, llvm_path


def group(name):
    family, task = name.split("::")
    if family == "Etc":
        return re.sub(r"(?<!^)(?=[A-Z])", "_", task).lower()
    return {"CSV": "csv", "CLBG": "clbg"}.get(family, family.lower())


def root_mib(name):
    if name in ("Binarytrees::Arena", "Maze::Generator", "Maze::BFS", "Maze::AStar"):
        return 256
    if group(name) in ("binarytrees", "graph"):
        return 128
    return 64


def build_hirsel(out, artifacts, rows, env=None):
    manifest = out / "inputs.json"
    manifest.write_text(json.dumps(inputs(ROOT, "harness")))
    for module, mib in sorted({(group(row["name"]), root_mib(row["name"])) for row in rows}):
        print(f"building {module} at {mib} MiB", flush=True)
        result = subprocess.run([
            ROOT / "tools/capped", artifacts["compiler"], "build",
            "--inputs", manifest, "--entry", f"langarena@1/{module}/main",
            "--store", out / "store", "--backend", "llvm", "--release",
            "--runtime", artifacts["runtime"], "--cc", llvm_path() / "bin/clang",
            "--dir", out, "--grant", "Stdout", "--grant", "Tasks", "--limit", str(mib << 20),
            "--json", "-o", out / f"{module}-{mib}",
        ], env={**(env or capped_env(out)), "LD_LIBRARY_PATH": str(llvm_path() / "lib")},
            stdout=subprocess.PIPE)
        (out / f"{module}-{mib}-build.json").write_bytes(result.stdout)
        result.check_returncode()


def oracle(out, binary, rows):
    # Running once against a sentinel exposes the unmodified Rust checksum.
    # Confirm the resulting checksum in all three binaries during sampling.
    fixture = out / "one-iteration.json"
    fixture.write_text(json.dumps(rows))
    for row in rows:
        name = row["name"]
        result = subprocess.run([ROOT / "tools/capped", binary, fixture, name],
                                cwd=out, env=capped_env(out), capture_output=True, text=True)
        prefix = re.escape(name)
        observed = re.search(rf"^{prefix}: ERR\[actual=(\d+), expected=0\]", result.stdout, re.M)
        zero = re.search(rf"^{prefix}: OK in ", result.stdout, re.M)
        if observed and result.returncode == 1:
            row["checksum"] = int(observed[1])
        elif not (zero and result.returncode == 0):
            raise ValueError(f"{name}: no Rust oracle\n{result.stdout}{result.stderr}")
    fixture.write_text(json.dumps(rows) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--config", type=Path, required=True, help="unchanged upstream run.js")
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--rust", type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    rows = [dict(row, iterations=1, warmup_iterations=0, checksum=0)
            for row in json.loads(args.config.read_text())]
    if args.artifacts:
        build_hirsel(out, json.loads(args.artifacts.read_text()), rows)
    if args.rust:
        oracle(out, args.rust.resolve(), rows)


if __name__ == "__main__":
    main()
