#!/usr/bin/env python3
"""Build and run the checkbox server from a Hirsel checkout."""
import argparse
import json
from pathlib import Path
import subprocess
import webbrowser

PROGRAM = Path(__file__).resolve().parent
ROOT = PROGRAM.parents[2]
ENTRY = "checkbox@1/main"


def inputs(work, source=PROGRAM / "main.hirsel"):
    store = json.loads((PROGRAM.parent / "store/manifest.json").read_text())["packages"][0]
    store["sources"] = [
        {**item, "file": str(ROOT / item["file"])}
        for item in store["sources"] if item["modules"] in (["store"], ["runner"], ["ingress"])
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


def build(compiler, work, web, listen, source=PROGRAM / "main.hirsel"):
    work.mkdir(parents=True, exist_ok=True)
    data = work / "data"
    data.mkdir(exist_ok=True)
    executable = work / "checkbox"
    port = int(listen.rsplit(":", 1)[1])
    subprocess.run([
        compiler, "build", "--source-core", "--inputs", inputs(work, source),
        "--store", work / "compiler-store", "--entry", ENTRY + "/main",
        "--dir", data, "--dir", web, "--grant", "Tasks", "--grant", "Clock",
        "--grant", "Random", "--grant", f"Net=Listen {listen.rsplit(':', 1)[0]}/32 {port}-{port}",
        "--limit", str(2 << 30), "-o", executable,
    ], check=True, cwd=ROOT)
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hirsel", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--web", type=Path, default=PROGRAM / "web")
    parser.add_argument("--listen", default="127.0.0.1:7700")
    parser.add_argument("--open", action="store_true")
    parser.add_argument("server_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    compiler, work, web = args.hirsel.resolve(), args.work.resolve(), args.web.resolve()
    executable = build(compiler, work, web, args.listen)
    url = f"http://{args.listen}/"
    print(url, flush=True)
    server_args = args.server_args[1:] if args.server_args[:1] == ["--"] else args.server_args
    with subprocess.Popen([executable, "--listen", args.listen, *server_args], cwd=ROOT) as server:
        if args.open:
            webbrowser.open(url)
        try:
            return server.wait()
        except KeyboardInterrupt:
            return server.wait()


if __name__ == "__main__":
    raise SystemExit(main())
