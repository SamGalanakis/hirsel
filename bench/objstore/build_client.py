"""Compile the std.http_client fixture against the shipping and allocation runtimes."""

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
    parser.add_argument("cert_dir", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256((args.cert_dir / "cert.pem").read_bytes()).hexdigest()
    paths = json.loads(subprocess.check_output([
        ROOT / ".buck2/bin/buck2", "build", "-j4", "--target-platforms",
        "//tools/buck2:release", "--show-json-output", *TARGETS,
    ], cwd=ROOT))
    compiler, shipping, counted = (ROOT / paths["root" + target] for target in TARGETS)
    cc = ROOT / "tools/buck2/toolchains/llvm-files/bin/hirsel-clang"
    manifest = out / "inputs.json"
    document = inputs(HERE / "client.hirsel")
    document["packages"][-1]["package"]["name"] = "objstore_bench"
    manifest.write_text(json.dumps(document))
    for name, archive, linker in [
        ("hirsel", shipping, cc),
        ("hirsel-allocations", counted, allocation_linker(cc, out)),
    ]:
        command = [
            compiler, "build", "--release", "--backend", "llvm", "--source-core",
            "--source-std", "--inputs", manifest, "--entry", "objstore_bench@1/main/main",
            "--runtime", archive / PLATFORM / "libhirsel_core_rt.a", "--cc", linker,
            "--store", out / "build-store", "-o", out / name,
        ]
        for grant in [
            "Clock", "Stdin", "Stdout",
            f"Net=Connect 127.0.0.1/32 1024-65535;Tls localhost 1024-65535 Roots=sha256:{digest}",
        ]:
            command += ["--grant", grant]
        subprocess.run(command, cwd=ROOT, check=True, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G"})


if __name__ == "__main__":
    main()
