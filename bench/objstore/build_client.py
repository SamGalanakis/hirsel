"""Compile the HTTP transfer client or the object protocol and abort probe."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
CORE_TOOLS = ROOT / "core/tools"
sys.path.insert(0, str(CORE_TOOLS))
from measurement_artifacts import allocation_linker
from omap_bench import inputs

PLATFORM = "x86_64-unknown-linux-gnu"
TARGETS = (
    "//:hirsel-core-driver-bin-hirsel",
    "//:hirsel-core-rt-archives",
    "//:hirsel-core-rt-allocation-count-archives",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cert_dir", type=Path, nargs="?")
    parser.add_argument("output", type=Path)
    parser.add_argument("--net-probe", action="store_true")
    args = parser.parse_args()
    if args.net_probe == (args.cert_dir is not None):
        parser.error("supply a certificate directory for the HTTP fixture, or --net-probe")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    targets = TARGETS[:2] if args.net_probe else TARGETS
    paths = json.loads(subprocess.check_output([
        ROOT / ".buck2/bin/buck2", "build", "-j4", "--target-platforms",
        "//tools/buck2:release", "--show-json-output", *targets,
    ], cwd=ROOT))
    compiler, shipping = (ROOT / paths["root" + target] for target in targets[:2])
    cc = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"
    manifest = out / "inputs.json"
    document = inputs(HERE / ("net_probe.hirsel" if args.net_probe else "client.hirsel"))
    document["packages"][-1]["package"]["name"] = "objstore_bench"
    manifest.write_text(json.dumps(document))
    grants = ["Clock", "Stdin", "Stdout"]
    connect = "Net=Connect 127.0.0.1/32 1024-65535"
    if args.net_probe:
        grants += ["Tasks", "Random", connect + ";Resolve localhost"]
        builds = [("net-probe", shipping, cc)]
    else:
        digest = hashlib.sha256((args.cert_dir / "cert.pem").read_bytes()).hexdigest()
        grants.append(connect + f";Tls localhost 1024-65535 Roots=sha256:{digest}")
        counted = ROOT / paths["root" + TARGETS[2]]
        builds = [("hirsel", shipping, cc), ("hirsel-allocations", counted, allocation_linker(cc, out))]
    for name, archive, linker in builds:
        # Keep the former 64 MiB default for both client workloads.
        command = [
            compiler, "build", "--release", "--backend", "llvm", "--source-core",
            "--source-std", "--inputs", manifest, "--entry", "objstore_bench@1/main/main",
            "--limit", str(64 << 20), "--runtime", archive / PLATFORM / "libhirsel_core_rt.a", "--cc", linker,
            "--store", out / "build-store", "-o", out / name,
        ]
        for grant in grants:
            command += ["--grant", grant]
        subprocess.run(command, cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})


if __name__ == "__main__":
    main()
