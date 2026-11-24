#!/usr/bin/env python3
"""Check, seed, or compare the core workspace's generated law locks."""

import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = ROOT / "core" / "programs"
FOREIGN_GRANTS = {
    "ffi_laws": ("Foreign=ffi_laws@1/laws/Laws",),
    "image_pipeline": (
        "Foreign=image_jpeg@1/image_jpeg/Jpeg",
        "Foreign=image_webp@1/webp/Webp",
        "Foreign=av1@1/av1/DecoderCodec",
        "Foreign=av1@1/av1/EncoderCodec",
        "Foreign=video_yuv@1/video_yuv/Yuv",
    ),
    "sqlite": ("Foreign=sqlite@1/sqlite/Sqlite",),
}


def manifest(directory):
    with (directory / "hirsel.toml").open("rb") as source:
        return tomllib.load(source)


def baseline_for(commit, package, destination):
    if commit is None:
        return []
    path = (package / "hirsel.laws").relative_to(ROOT).as_posix()
    present = subprocess.check_output(["git", "ls-tree", commit, "--", path], cwd=ROOT)
    if not present:
        return []
    destination.write_bytes(subprocess.check_output(["git", "show", f"{commit}:{path}"], cwd=ROOT))
    return ["--laws-baseline", str(destination)]


def run_package(args, member, env, commit, compiler_scratch):
    package = WORKSPACE / member
    inputs = [str(package), "--libraries", str(ROOT / "core")]
    if args.mode in ("check", "seed"):
        action = "check" if args.mode == "check" else "write"
        lock_store = (compiler_scratch or Path(env["TMPDIR"])) / "store"
        command = [str(args.compiler), "laws", action, *inputs, "--store", str(lock_store)]
        status = subprocess.run(command, cwd=ROOT, env=env).returncode
        if status or args.mode == "check":
            return status

    logs = args.logs / member
    logs.mkdir(parents=True, exist_ok=True)
    command = [str(args.compiler), "test", str(package),
               "--laws", "--json", "--test-timeout", "300"]
    if compiler_scratch is not None:
        command.extend(["--store", str(compiler_scratch / member)])
    if "tests" not in manifest(package):
        command += ["--limit", "67108864"]
    for grant in FOREIGN_GRANTS.get(member, ()):
        command += ["--grant", grant]
    if args.mode == "seed":
        command.append("--write-laws")
    else:
        baseline = baseline_for(commit, package, logs / "baseline.laws")
        if not baseline:
            print(f"{member}: no previous lock", flush=True)
        command.extend(baseline)

    output = logs / "tests.json"
    with output.open("w") as stdout, (logs / "stderr.log").open("w") as stderr:
        status = subprocess.run(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr).returncode
    try:
        report = json.loads(output.read_text())
    except ValueError:
        report = {}
    regressions = report.get("law_regressions", [])
    empty = status == 2 and report.get("tests") == [] and not regressions
    print(f"{member}: {'no tests' if empty else f'exit {status}'}", flush=True)
    for red in regressions:
        print(f"  {red}", flush=True)
    return 0 if empty else status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("check", "seed", "nightly"))
    parser.add_argument("--compiler", type=Path, required=True)
    parser.add_argument("--commit")
    parser.add_argument("--logs", type=Path)
    parser.add_argument("--compiler-scratch", type=Path,
                        help="Parent directory for build stores and caches. Test directories still use TMPDIR.")
    args = parser.parse_args()
    if args.mode != "check" and args.logs is None:
        parser.error("--logs is required for seed and nightly")
    args.compiler = args.compiler.absolute()
    if args.logs is not None:
        args.logs = args.logs.absolute()
    commit = None
    if args.commit:
        commit = subprocess.check_output(
            ["git", "rev-parse", "--verify", f"{args.commit}^{{commit}}"], cwd=ROOT, text=True,
        ).strip()
    status = 0
    with ExitStack() as stack:
        temp = stack.enter_context(tempfile.TemporaryDirectory(prefix="hirsel-laws-", dir=os.getenv("TMPDIR", "/dev/shm")))
        compiler_scratch = None
        if args.compiler_scratch is not None:
            compiler_scratch = Path(stack.enter_context(tempfile.TemporaryDirectory(
                prefix="hirsel-law-builds-", dir=args.compiler_scratch.absolute())))
        cache = (compiler_scratch or Path(temp)) / "cache"
        env = os.environ | {"TMPDIR": temp, "XDG_CACHE_HOME": str(cache)}
        for member in manifest(WORKSPACE)["workspace"]["members"]:
            status |= int(run_package(args, member, env, commit, compiler_scratch) != 0)
    return status


if __name__ == "__main__":
    sys.exit(main())
