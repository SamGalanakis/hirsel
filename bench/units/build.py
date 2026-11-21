#!/usr/bin/env python3
"""Build one mc-units-proto hirsel program (throwaway prototype)."""
import json, os, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "bench/multicore"))
import build as mc  # noqa: E402

def main():
    src, out, compiler, runtime = map(Path, sys.argv[1:5])
    grants = sys.argv[5:]
    out.mkdir(parents=True, exist_ok=True)
    case = src.stem
    inputs = out / f"{case}.json"
    data = mc.hirsel_inputs(src)
    inputs.write_text(json.dumps(data))
    llvm = ROOT / "tools/buck2/toolchains/llvm-files"
    env = os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "1",
                        "LD_LIBRARY_PATH": str(llvm / "lib")}
    net = ("Net=Listen 127.77.0.0/16 0-65535;Connect 127.77.0.0/16 0-65535;"
           "Listen 127.0.0.1/32 0-65535;Connect 127.0.0.1/32 0-65535")
    record = os.environ.get("UNITS_RECORD")
    cmd = [ROOT / "tools/capped", compiler, "run" if record else "build", "--inputs", inputs,
           "--entry", f"multicore@1/{case}/main", "--backend", "llvm", "--release",
           "--runtime", runtime, "--cc", llvm / "bin/clang", "--store", out / "store",
           "--grant", "Tasks", "--grant", "Clock",
           "--grant", "Stderr", "--limit", os.environ.get("UNITS_LIMIT", "1073741824"),
           "--debug-lines", "on", "--json", "-o", out / case]
    # Grant Net and a directory only to a program whose entry takes them.
    text = src.read_text()
    if "Net" in text:
        cmd += ["--grant", net]
    if "Dir" in text:
        cmd += ["--dir", os.environ.get("UNITS_DIR", str(out))]
    for g in grants:
        cmd += ["--grant", g]
    if record:
        # Build a recording executable (run --record), then let it run once
        # with the given arguments; later runs reuse the executable from a
        # fresh working directory with the relative tape path.
        cmd += ["--record", record, "--"] + os.environ.get("UNITS_RECORD_ARGS", "").split()
    r = subprocess.run(list(map(str, cmd)), env=env, capture_output=True, text=True)
    print(r.stdout[-6000:]); print(r.stderr[-6000:])
    sys.exit(r.returncode)

main()
