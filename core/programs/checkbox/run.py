#!/usr/bin/env python3
"""Build and run the checkbox server from a Hirsel checkout."""
import argparse
import json
import shutil
import socket
from pathlib import Path
import subprocess
import time
import tomllib
import webbrowser

PROGRAM = Path(__file__).resolve().parent
ROOT = PROGRAM.parents[2]
ENTRY = "checkbox@1/main"
N_PLACE = "1048576"


def inputs(work, source=PROGRAM / "main.hirsel", source_overrides=None):
    """Core, std, the store and runner packages with the modules checkbox binds, and checkbox;
    `source_overrides` maps an original source path to a replacement file."""
    source_overrides = source_overrides or {}
    core = {"name": "core", "version": "1"}
    std = {"name": "std", "version": "1"}
    packages = []
    for name, dependencies in [("core", []), ("std", [core])]:
        library = ROOT / "core" / name
        packages.append({
            "package": {"name": name, "version": "1"}, "dependencies": dependencies,
            "sources": [
                {"file": str(file), "modules": list(relative.with_suffix("").parts), "signatures": name == "core"}
                for file in sorted(library.rglob("*.hirsel"))
                if (relative := file.relative_to(library)).parts[0] != "tests"
            ],
        })
    bound = []
    for name in ("store", "runner"):
        directory = PROGRAM.parent / name
        with (directory / "hirsel.toml").open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)
        package = manifest["package"]
        program = manifest.get("program", {}).get("module")
        packages.append({
            "package": {"name": package["name"], "version": str(package["major"])},
            "dependencies": [core] + [
                {"name": name, "version": str(dep["major"])}
                for name, dep in manifest["dependencies"].items()
            ],
            "sources": [
                {"file": str(source_overrides.get(directory / file, directory / file)),
                 "modules": module.split("."), "signatures": False}
                for file, module in manifest["sources"].items()
                if module != program
            ],
        })
        bound.append(packages[-1]["package"])
    packages.append({
        "package": {"name": "checkbox", "version": "1"},
        "dependencies": [core, std, *bound],
        "sources": [{"file": str(source), "modules": ["main"], "signatures": False}],
    })
    path = work / "inputs.json"
    path.write_text(json.dumps({"core": core, "packages": packages}) + "\n")
    return path


def build(compiler, work, web, listen, source=PROGRAM / "main.hirsel", source_overrides=None, *,
          prefix=(), flags=(), env=None, stdout=None, entry="main"):
    work.mkdir(parents=True, exist_ok=True)
    data = work / "data"
    data.mkdir(exist_ok=True)
    executable = work / "checkbox"
    port = int(listen.rsplit(":", 1)[1])
    subprocess.run([
        *prefix, compiler, "build", *flags, "--source-core", "--inputs", inputs(work, source, source_overrides),
        "--store", work / "compiler-store", "--entry", ENTRY + "/" + entry,
        "--dir", data, "--dir", web, "--grant", "Tasks", "--grant", "Clock", "--grant", "Process", "--grant", "Stderr",
        "--grant", "Random", "--grant", f"Net=Listen {listen.rsplit(':', 1)[0]}/32 {port}-{port}",
        "--limit", str(3 << 30), "-o", executable,
    ], check=True, cwd=ROOT, env=env, stdout=stdout)
    return executable


def prepare_web(compiler, executable, web):
    subprocess.run([
        compiler, "sdk", "ts", executable, "--output", web, "--n-place", N_PLACE,
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
