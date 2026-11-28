#!/usr/bin/env python3
"""Link the soak Place against the checkout's Store and shipping glibc runtime."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core/tools"))
from store_tests import inputs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("hirsel", type=Path)
    parser.add_argument("runtime", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--cc", type=Path, required=True)
    args = parser.parse_args()
    corpus = inputs(ROOT)
    corpus["packages"][-1]["sources"].append({
        "file": str(HERE / "place.hirsel"), "modules": ["store"], "signatures": False,
    })
    with tempfile.TemporaryDirectory(prefix="store-soak-build-") as scratch:
        manifest = Path(scratch) / "inputs.json"
        manifest.write_text(json.dumps(corpus))
        subprocess.run([
            args.hirsel.resolve(), "build", "--inputs", manifest, "--entry", "store@1/store/soak_place",
            "--source-core", "--source-std", "--backend", "llvm", "--limit", "2147483648",
            "--runtime", args.runtime.resolve(), "--cc", args.cc.resolve(), "--dir", "/proc/self/cwd",
            "--grant", "Clock", "--grant", "Random", "--grant", "Tasks", "--grant", "Stdin",
            "--grant", "Stdout",
            "--grant", "Net=Resolve 127.0.0.1;Connect 127.0.0.1/32 1-65535",
            "--grant", "Secret:archive=env:SOAK_ARCHIVE_SECRET kdf",
            "-o", args.output.resolve(),
        ], cwd=ROOT, env=dict(os.environ, TMPDIR=scratch), check=True)


if __name__ == "__main__":
    main()
