#!/usr/bin/env python3
"""Run WebTransport T5 and T10 on Boat, using a bare Hirsel driver and PIC runtime."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


HERE = Path(__file__).resolve().parent
CORE = HERE.parents[2]
DISPATCH = Path("/workspace/code/hirsel-briefs/tools/boat-dispatch")
T10 = "webtransport@1/laws/certificate_rotation_accepts_either"
ENTRY = "webtransport_browser_echo@1/main/main"
PLAYWRIGHT = "1.63.0"


def copy_sources(source, target, globs):
    files = {file for pattern in globs for file in source.glob(pattern) if file.is_file()}
    for file in sorted(files):
        dest = target / file.relative_to(source)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, dest)


def stage(work, compiler, runtime, linker):
    bundle = work / "bundle"
    bundle.mkdir()
    tools = bundle / "toolchain"
    tools.mkdir()
    for source, name in [(compiler, "hirsel"), (runtime, "runtime.a"), (linker, "ld.lld")]:
        shutil.copy2(source, tools / name)
    for name, globs in [("core", ["*.hirsel"]), ("std", ["*.hirsel", "sim/*.hirsel"]),
                         ("programs/quic", ["*.hirsel", "hirsel.toml", "hirsel.laws"]),
                         ("programs/webtransport", ["*.hirsel", "hirsel.toml", "hirsel.laws",
                                                    "browser/*.hirsel", "browser/hirsel.toml", "browser/*.mjs"])]:
        copy_sources(CORE / name, bundle / "core" / name, globs)
    (bundle / "core/programs/hirsel.toml").write_text(
        '[workspace]\nmembers = ["quic", "webtransport", "webtransport/browser"]\n'
    )
    return bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--linker", required=True, type=Path)
    parser.add_argument("--law", choices=("t5", "t10"), help="select one law; default runs both")
    parser.add_argument("--work", required=True, type=Path)
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    bundle = stage(work, args.compiler, args.runtime, args.linker)
    tools = bundle / "toolchain"
    if args.law != "t10":
        subprocess.run([
            tools / "hirsel", "build", "core/programs/webtransport/browser", "--libraries", "core",
            "--cc", "/usr/bin/gcc", "--runtime", tools / "runtime.a", "--entry", ENTRY,
            "--store", work / "build-store", "--limit", "268435456", "--grant", "Random", "--grant", "Clock", "--grant", "Stdout",
            "--grant", "Net=UdpBind 127.0.0.1/32 0-65535;UdpSend 127.0.0.1/32 1-65535",
            "-o", "echo",
        ], cwd=bundle, env=dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}"), check=True)
    common = '--libraries core --cc /usr/bin/gcc --runtime "$PWD/toolchain/runtime.a"'
    package = "core/programs/webtransport"
    rotation = (
        'mkdir -p /dev/shm/hirsel-wt-rotation && export TMPDIR=/dev/shm/hirsel-wt-rotation && '
        f'hirsel test {package} {common} --exact {T10} --test-timeout 300 --rerun -j 1'
    )
    echo = f"""
set -e
command -v npm >/dev/null || (sudo -n apt-get update -qq && sudo -n apt-get install -y nodejs npm)
npm install --ignore-scripts --no-save --no-audit --no-fund playwright-core@{PLAYWRIGHT}
node_modules/.bin/playwright-core install --with-deps chromium firefox
node {package}/browser/browser_echo.mjs "$PWD/echo"
"""
    jobs = [
        {"lane": "webtransport-t10", "bundle": "bundle", "command": rotation, "seconds": 180},
        {"lane": "webtransport-t5", "bundle": "bundle", "command": echo, "seconds": 600},
    ]
    if args.law:
        jobs = [job for job in jobs if job["lane"] == f"webtransport-{args.law}"]
    manifest = work / "jobs.json"
    manifest.write_text(json.dumps(jobs) + "\n")
    return subprocess.call([DISPATCH, "--manifest", manifest, "--batch-seconds", "700"], cwd=work)


if __name__ == "__main__":
    raise SystemExit(main())
