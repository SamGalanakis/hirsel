"""Link the UDP comparison workloads with the same release runtime."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core" / "tools"))
from omap_bench import inputs

COMPILER = "//:hirsel-core-driver-bin-hirsel"
RUNTIME = "//:hirsel-core-rt-archives"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--outputs", type=Path,
                        help="Reuse the JSON output of the compiler and runtime build")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if args.outputs is None:
        paths = json.loads(subprocess.check_output([
            ROOT / ".buck2/bin/buck2", "build", "-j4", "--show-json-output",
            "--target-platforms", "//tools/buck2:release", COMPILER, RUNTIME,
        ], cwd=ROOT))
    else:
        paths = json.loads(args.outputs.read_text())
    compiler = ROOT / paths["root" + COMPILER]
    runtime = ROOT / paths["root" + RUNTIME] / "x86_64-unknown-linux-gnu" / "libhirsel_core_rt.a"
    document = inputs(HERE / "load.hirsel")
    document["packages"][-1]["package"]["name"] = "udp_comparison"
    manifest = output / "inputs.json"
    manifest.write_text(json.dumps(document))
    linker = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"
    subprocess.run([
        compiler, "build", "--release", "--backend", "llvm", "--source-core", "--source-std",
        "--inputs", manifest, "--entry", "udp_comparison@1/main/main", "--runtime", runtime,
        "--cc", linker, "--store", output / "build-store", "-o", output / "load",
        "--debug-lines", "on", "--grant", "Clock", "--grant", "Stdout", "--grant", "Stdin", "--grant", "Tasks",
        "--grant", "Net=UdpBind 127.0.0.1/32 1-65535;UdpSend 127.0.0.1/32 1-65535",
    ], cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})
    subprocess.run([
        "cc", "-O2", "-std=c17", "-Wall", "-Wextra", "-Werror", HERE / "send_flows.c",
        "-o", output / "send-flows",
    ], check=True)


if __name__ == "__main__":
    main()
