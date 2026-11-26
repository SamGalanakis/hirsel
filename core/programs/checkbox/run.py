#!/usr/bin/env python3
"""Build and run the checkbox server from a Hirsel checkout."""
import argparse
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


def build(compiler, work, web, listen, libraries=ROOT / "core", *,
          prefix=(), flags=(), env=None, stdout=None, entry="main"):
    """Build the workspace member checkbox from `libraries` (default: the checkout)."""
    work.mkdir(parents=True, exist_ok=True)
    data = work / "data"
    data.mkdir(exist_ok=True)
    executable = work / "checkbox"
    port = int(listen.rsplit(":", 1)[1])
    subprocess.run([
        *prefix, compiler, "build", libraries / "programs/checkbox", "--libraries", libraries, *flags, "--source-core",
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
