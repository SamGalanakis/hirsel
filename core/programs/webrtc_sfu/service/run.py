#!/usr/bin/env python3
"""Build and run the SFU deployment (webrtc.md N1): the room place, signal.serve and the
TURN relay in one Hirsel process on 127.0.0.1, serving the call page at http://<listen>/.

    python3 core/programs/webrtc_sfu/service/run.py --hirsel <hirsel> --work /tmp/sfu
"""
import argparse
import json
import shutil
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path

PROGRAM = Path(__file__).resolve().parent
ROOT = PROGRAM.parents[3]
ENTRY = "webrtc_sfu_service@1/main/main"
N_PLACE = "1048576"


def library(name):
    directory = ROOT / "core" / name
    return [
        {"file": str(file), "modules": list(relative.with_suffix("").parts), "signatures": name == "core"}
        for file in sorted(directory.rglob("*.hirsel"))
        if (relative := file.relative_to(directory)).parts[0] != "tests"
    ]


def inputs(work):
    """Core, std and every package the service reaches through path dependencies."""
    core, std = {"name": "core", "version": "1"}, {"name": "std", "version": "1"}
    packages = [
        {"package": core, "dependencies": [], "sources": library("core")},
        {"package": std, "dependencies": [core], "sources": library("std")},
    ]
    done = {}

    def visit(directory):
        directory = directory.resolve()
        if directory in done:
            return done[directory]
        manifest = tomllib.loads((directory / "hirsel.toml").read_text())
        dependencies = [core]
        for name, dep in manifest.get("dependencies", {}).items():
            dependencies.append(visit(directory / dep["path"]) if "path" in dep else {"name": name, "version": str(dep["major"])})
        program = manifest.get("program", {}).get("module")
        package = {"name": manifest["package"]["name"], "version": str(manifest["package"]["major"])}
        packages.append({"package": package, "dependencies": dependencies, "sources": [
            {"file": str(directory / file), "modules": module.split("."), "signatures": False}
            for file, module in manifest["sources"].items()
            if module != program or directory == PROGRAM
        ]})
        done[directory] = package
        return package

    visit(PROGRAM)
    path = work / "inputs.json"
    path.write_text(json.dumps({"core": core, "packages": packages}) + "\n")
    return path


def build(compiler, work, web, prefix=()):
    work.mkdir(parents=True, exist_ok=True)
    executable = work / "sfu"
    subprocess.run([
        *prefix, compiler, "build", "--source-core", "--inputs", inputs(work), "--store", work / "compiler-store",
        "--entry", ENTRY, "--dir", web,
        "--grant", "Tasks", "--grant", "Clock", "--grant", "Random", "--grant", "Stdout",
        "--grant", "Net=Listen 127.0.0.1/32 0-65535;UdpBind 127.0.0.1/32 0-65535;UdpSend 127.0.0.1/32 1-65535;"
                   "Connect 127.0.0.1/32 1-65535",
        "--limit", str(4 << 30), "-o", executable,
    ], check=True, cwd=ROOT)
    return executable


def prepare_web(compiler, executable, web):
    """The SDK bindings `hirsel sdk ts` writes for this place, the page, and its bundle."""
    generated = web / "generated"
    generated.mkdir(parents=True, exist_ok=True)
    subprocess.run([compiler, "sdk", "ts", executable, "--output", generated, "--n-place", N_PLACE], check=True, cwd=ROOT)
    shutil.copyfile(PROGRAM / "index.html", web / "index.html")
    subprocess.run([
        "npm", "exec", "--yes", "--package=esbuild@0.25.12", "--", "esbuild", PROGRAM / "page.ts", "--bundle",
        "--format=esm", f"--alias:generated={generated}", f"--outfile={web / 'client.js'}",
    ], check=True, cwd=ROOT)


def command(executable, listen, args):
    return [executable, f"--hirsel-n-place={N_PLACE}", "--listen", listen, *args]


def wait_for_listener(server, listen, seconds=60):
    host, port = listen.rsplit(":", 1)
    end = time.monotonic() + seconds
    while True:
        if server.poll() is not None:
            raise RuntimeError(f"service exited before listening (status {server.returncode})")
        if time.monotonic() > end:
            raise RuntimeError("service did not listen")
        try:
            with socket.create_connection((host, int(port))):
                return
        except OSError:
            time.sleep(0.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--hirsel", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--listen", default="127.0.0.1:7800")
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("service_args", nargs=argparse.REMAINDER,
                        help="after --: --media PORT --units N --turn PORT --audio-forward N --seconds N")
    args = parser.parse_args()
    compiler, work = args.hirsel.resolve(), args.work.resolve()
    web = work / "web"
    web.mkdir(parents=True, exist_ok=True)
    executable = build(compiler, work, web)
    prepare_web(compiler, executable, web)
    if args.build_only:
        print(executable)
        return 0
    rest = args.service_args[1:] if args.service_args[:1] == ["--"] else args.service_args
    with subprocess.Popen(command(executable, args.listen, rest), cwd=ROOT) as server:
        wait_for_listener(server, args.listen)
        print(f"http://{args.listen}/", flush=True)
        try:
            return server.wait()
        except KeyboardInterrupt:
            return server.wait()


if __name__ == "__main__":
    sys.exit(main())
