#!/usr/bin/env python3
"""Build and run the checkbox server from a Hirsel checkout."""
import argparse
import json
import shutil
import socket
from pathlib import Path
import subprocess
import time
import webbrowser

PROGRAM = Path(__file__).resolve().parent
ROOT = PROGRAM.parents[2]
ENTRY = "checkbox@1/main"
N_PLACE = "1048576"


def inputs(work, source=PROGRAM / "main.hirsel", store_sources=None):
    store = json.loads((PROGRAM.parent / "store/manifest.json").read_text())["packages"][0]
    store_sources = store_sources or {}
    store["sources"] = [
        {**item, "file": str(store_sources.get(item["modules"][0], ROOT / item["file"]))}
        for item in store["sources"] if item["modules"] in (["store"], ["triggers"], ["runner"], ["ingress"])
    ]
    core = {"name": "core", "version": "1"}
    std = {"name": "std", "version": "1"}
    packages = []
    for name, dependencies in [("core", []), ("std", [core])]:
        packages.append({
            "package": {"name": name, "version": "1"}, "dependencies": dependencies,
            "sources": [
                {"file": str(file), "modules": [file.stem], "signatures": name == "core"}
                for file in sorted((ROOT / "core" / name).glob("*.hirsel"))
            ],
        })
    packages.extend([store, {
        "package": {"name": "checkbox", "version": "1"},
        "dependencies": [core, std, store["package"]],
        "sources": [{"file": str(source), "modules": ["main"], "signatures": False}],
    }])
    path = work / "inputs.json"
    path.write_text(json.dumps({"core": core, "packages": packages}) + "\n")
    return path


def build(compiler, work, web, listen, source=PROGRAM / "main.hirsel", store_sources=None, *,
          prefix=(), flags=(), env=None, stdout=None):
    work.mkdir(parents=True, exist_ok=True)
    data = work / "data"
    data.mkdir(exist_ok=True)
    executable = work / "checkbox"
    port = int(listen.rsplit(":", 1)[1])
    subprocess.run([
        *prefix, compiler, "build", *flags, "--source-core", "--inputs", inputs(work, source, store_sources),
        "--store", work / "compiler-store", "--entry", ENTRY + "/main",
        "--dir", data, "--dir", web, "--grant", "Tasks", "--grant", "Clock", "--grant", "Process",
        "--grant", "Random", "--grant", f"Net=Listen {listen.rsplit(':', 1)[0]}/32 {port}-{port}",
        "--limit", str(3 << 30), "-o", executable,
    ], check=True, cwd=ROOT, env=env, stdout=stdout)
    return executable


def prepare_web(compiler, executable, web):
    subprocess.run([
        compiler, "sdk", "ts", executable, "--out", web, "--n-place", N_PLACE,
    ], check=True, cwd=ROOT)
    shutil.copyfile(PROGRAM / "web/index.html", web / "index.html")
    subprocess.run([
        "npm", "exec", "--yes", "--package=esbuild@0.25.12", "--", "esbuild",
        PROGRAM / "browser-client.ts", "--bundle", "--format=esm",
        f"--outfile={web / 'client.js'}",
    ], check=True, cwd=ROOT)



def command(executable, listen, args):
    return [executable, f"--hirsel-n-place={N_PLACE}", "--listen", listen, *args]


def wait_for_listener(server, listen):
    host, port = listen.rsplit(":", 1)
    while True:
        status = server.poll()
        if status is not None:
            raise RuntimeError(f"server exited before listen (status {status})")
        try:
            with socket.create_connection((host, int(port))):
                return
        except ConnectionRefusedError:
            time.sleep(0.05)



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hirsel", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--web", type=Path, help="serve an existing web directory")
    parser.add_argument("--listen", default="127.0.0.1:7700")
    parser.add_argument("--open", action="store_true")
    parser.add_argument("server_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    compiler, work = args.hirsel.resolve(), args.work.resolve()
    web = args.web.resolve() if args.web else work / "web"
    web.mkdir(parents=True, exist_ok=True)
    executable = build(compiler, work, web, args.listen)
    if args.web is None:
        prepare_web(compiler, executable, web)
    url = f"http://{args.listen}/"
    server_args = args.server_args[1:] if args.server_args[:1] == ["--"] else args.server_args
    with subprocess.Popen(command(executable, args.listen, server_args), cwd=ROOT) as server:
        wait_for_listener(server, args.listen)
        print(url, flush=True)
        if args.open:
            webbrowser.open(url)
        try:
            return server.wait()
        except KeyboardInterrupt:
            return server.wait()


if __name__ == "__main__":
    raise SystemExit(main())
