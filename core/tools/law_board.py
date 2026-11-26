#!/usr/bin/env python3
"""Check, seed, or compare the core workspace's generated law locks.

`check` covers every workspace member and every directory with a tracked
`hirsel.laws` (the `*/vectors` test packages). `check --changed-since BASE`
checks only those whose lock could move: a package is selected when a path
changed since BASE (worktree and untracked files included) lies in it or in a
package it reaches through `path` dependencies (a vectors package reaches its
parent). A change under any of ALL_TRIGGERS selects every package: std, the
core library, the workspace manifest and lock, the crate sources that compute
the lock's hashes and this file."""

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
    "h264": ("Foreign=h264@1/h264/H264",),
    "image_pipeline": (
        "Foreign=image_jpeg@1/image_jpeg/Jpeg",
        "Foreign=image_webp@1/webp/Webp",
        "Foreign=av1@1/av1/DecoderCodec",
        "Foreign=av1@1/av1/EncoderCodec",
        "Foreign=video_yuv@1/video_yuv/Yuv",
    ),
    "sqlite": ("Foreign=sqlite@1/sqlite/Sqlite",),
}
# A full check is about 400 s cold, so a general compiler change does not select
# everything; only the code that computes the lock columns does. A compiler change
# that moves digests elsewhere is caught by the next check that selects the package.
# The crate sources that compute a lock's columns: src and sig (law_lock), the
# header and signature digests sig records (reuse.rs), their hash framing and
# definition identity (hirsel-core-ids), the canonical token stream they hash
# (token kinds, lexer, syntax tree) and the policy tuple run records
# (native_entry.rs). Any other compiler change that moves a lock is caught by
# the nightly's full check.
ALL_TRIGGERS = (
    "core/std/", "core/core/",
    "core/programs/hirsel.toml", "core/programs/hirsel.lock",
    "crates/hirsel-core-driver/src/law_lock", "crates/hirsel-core-driver/src/reuse.rs",
    "crates/hirsel-core-driver/src/native_entry.rs", "crates/hirsel-core-ids/src/lib.rs",
    "crates/hirsel-core-syntax/src/token.rs", "crates/hirsel-core-syntax/src/lexer.rs",
    "crates/hirsel-core-syntax/src/lib.rs", "core/tools/law_board.py",
)


def manifest(directory):
    with (directory / "hirsel.toml").open("rb") as source:
        return tomllib.load(source)


def package_manifest(directory):
    for name in ("hirsel.toml", "test.toml"):
        if (directory / name).is_file():
            with (directory / name).open("rb") as source:
                return tomllib.load(source)
    return {}


def lock_packages():
    """Workspace members and every directory holding a tracked hirsel.laws."""
    members = set(manifest(WORKSPACE)["workspace"]["members"])
    tracked = subprocess.check_output(
        ["git", "ls-files", "-z", "--", "core/programs/**/hirsel.laws"], cwd=ROOT, text=True)
    members.update((ROOT / path).parent.relative_to(WORKSPACE).as_posix()
                   for path in tracked.split("\0") if path)
    return sorted(members)


def reached_directories(member):
    """The package directory and every directory it reaches by path dependency."""
    start = (WORKSPACE / member).resolve()
    seen, pending = set(), [start]
    while pending:
        directory = pending.pop()
        if directory in seen:
            continue
        seen.add(directory)
        if (directory / "test.toml").is_file() and (directory.parent / "hirsel.toml").is_file():
            pending.append(directory.parent)
        for binding in package_manifest(directory).get("dependencies", {}).values():
            if isinstance(binding, dict) and "path" in binding:
                pending.append((directory / binding["path"]).resolve())
    return [directory.relative_to(ROOT).as_posix() + "/" for directory in seen]


def changed_paths(base):
    merge_base = subprocess.check_output(["git", "merge-base", "HEAD", base], cwd=ROOT, text=True).strip()
    diff = subprocess.check_output(
        ["git", "diff", "--name-only", "--no-renames", "-z", merge_base], cwd=ROOT, text=True)
    untracked = subprocess.check_output(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT, text=True)
    return {path for path in (diff + "\0" + untracked).split("\0") if path}


def selected_packages(base):
    packages = lock_packages()
    changed = changed_paths(base)
    if any(path.startswith(ALL_TRIGGERS) for path in changed):
        return packages
    return [member for member in packages
            if any(path.startswith(tuple(reached_directories(member))) for path in changed)]


def baseline_for(commit, package, destination):
    if commit is None:
        return []
    path = (package / "hirsel.laws").relative_to(ROOT).as_posix()
    present = subprocess.check_output(["git", "ls-tree", commit, "--", path], cwd=ROOT)
    if not present:
        return []
    destination.write_bytes(subprocess.check_output(["git", "show", f"{commit}:{path}"], cwd=ROOT))
    return ["--laws-baseline", str(destination)]


def incomplete_lock_rows(package):
    """Rows whose bug/oracle provenance pair is half-written."""
    path = package / "hirsel.laws"
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text().splitlines()[1:]:
        fields = dict(field.split("=", 1) for field in line.split()[1:])
        missing = [name for name in ("bug", "oracle") if fields.get(name) == "-"]
        if len(missing) == 1:
            rows.append(f"{line.split()[0]}: missing {'/'.join(missing)} provenance")
    return rows


def run_package(args, member, env, commit, compiler_scratch):
    package = WORKSPACE / member
    inputs = [str(package), "--libraries", str(ROOT / "core")]
    if args.mode in ("check", "seed"):
        action = "check" if args.mode == "check" else "write"
        lock_store = (compiler_scratch or Path(env["TMPDIR"])) / "store"
        command = [str(args.compiler), "laws", action, *inputs, "--store", str(lock_store)]
        status = subprocess.run(command, cwd=ROOT, env=env).returncode
        if args.mode == "check":
            incomplete = incomplete_lock_rows(package)
            for row in incomplete:
                print(f"{member}: {row}", flush=True)
            return int(bool(status or incomplete))
        if status:
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
    parser.add_argument("--changed-since", metavar="BASE",
                        help="check: only packages whose lock a change since merge-base(HEAD, BASE) can move")
    parser.add_argument("--compiler-scratch", type=Path,
                        help="Parent directory for build stores and caches. Test directories still use TMPDIR.")
    args = parser.parse_args()
    if args.mode != "check" and args.logs is None:
        parser.error("--logs is required for seed and nightly")
    if args.changed_since and args.mode != "check":
        parser.error("--changed-since is for check")
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
        if args.mode != "check":
            members = manifest(WORKSPACE)["workspace"]["members"]
        elif args.changed_since:
            members = selected_packages(args.changed_since)
        else:
            members = lock_packages()
        if args.mode == "check":
            print(f"laws check: {len(members)} package(s)", flush=True)
        for member in members:
            status |= int(run_package(args, member, env, commit, compiler_scratch) != 0)
    return status


if __name__ == "__main__":
    sys.exit(main())
