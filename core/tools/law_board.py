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
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/toolchain-dist"))
import hcfg

WORKSPACE = ROOT / "core" / "programs"
FOREIGN_GRANTS = {
    "ffi_laws": ("Foreign=hirsel.ai/ffi_laws@1/laws/Laws",),
    "h264": ("Foreign=hirsel.ai/h264@1/h264/H264",),
    "image_pipeline": (
        "Foreign=hirsel.ai/image_jpeg@1/image_jpeg/Jpeg",
        "Foreign=hirsel.ai/image_webp@1/webp/Webp",
        "Foreign=hirsel.ai/av1@1/av1/DecoderCodec",
        "Foreign=hirsel.ai/av1@1/av1/EncoderCodec",
        "Foreign=hirsel.ai/video_yuv@1/video_yuv/Yuv",
    ),
    "sqlite": ("Foreign=hirsel.ai/sqlite@1/sqlite/Sqlite",),
}
# Package IDs renamed by aa5cd037. A pre-cutover baseline still covers these laws.
SCOPED_LAW_PACKAGES = frozenset((
    "archive", "ffi_laws", "h264", "image_core", "image_pipeline", "jwtbind",
    "postgres", "spatial", "sqlite", "toml", "tz", "vector", "video_packager",
    "xml", "yaml",
))
STORE_LAWS_MOVED_TO_MANIFEST = (
    "disk_test_page_emission",
    "disk_test_page_location_closure",
    "disk_test_paged_manifest",
    "test_l69_lineage_last_holder_ghost_and_adopt",
)
OLD_REPLAY_TEST = "srtp@1/srtp/replay_reference_model"
REPLAY_TEST = "hirsel.ai/replay_receive_laws@1/laws/replay_reference_model"
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
    "core/programs/hirsel.hcfg", "core/programs/hirsel.lock",
    "crates/hirsel-core-driver/src/law_lock", "crates/hirsel-core-driver/src/reuse.rs",
    "crates/hirsel-core-driver/src/native_entry.rs", "crates/hirsel-core-ids/src/lib.rs",
    "crates/hirsel-core-syntax/src/token.rs", "crates/hirsel-core-syntax/src/lexer.rs",
    "crates/hirsel-core-syntax/src/lib.rs", "core/tools/law_board.py",
)


def members(compiler):
    return hcfg.read(WORKSPACE / "hirsel.hcfg", "pkg_v1.Workspace", compiler)["members"]


def lock_packages(compiler):
    """Workspace members and every directory holding a tracked hirsel.laws."""
    listed = set(members(compiler))
    tracked = subprocess.check_output(
        ["git", "ls-files", "-z", "--", "core/programs/**/hirsel.laws"], cwd=ROOT, text=True)
    listed.update((ROOT / path).parent.relative_to(WORKSPACE).as_posix()
                   for path in tracked.split("\0") if path)
    return sorted(listed)


def reached_directories(member, compiler):
    """The package directory and every directory it reaches by path dependency."""
    start = (WORKSPACE / member).resolve()
    seen, pending = set(), [start]
    while pending:
        directory = pending.pop()
        if directory in seen:
            continue
        seen.add(directory)
        vector = directory / "test.hcfg"
        manifest = directory / "hirsel.hcfg"
        if vector.is_file():
            pending.append(directory.parent)
            data = hcfg.read(vector, "pkg_v1.VectorTests", compiler)
        elif manifest.is_file():
            data = hcfg.read(manifest, "pkg_v1.Package", compiler)
        else:
            continue
        for _, binding in data["dependencies"]:
            if binding["case"] == "Path":
                pending.append((directory / binding["fields"]["path"]).resolve())
    return [directory.relative_to(ROOT).as_posix() + "/" for directory in seen]


def changed_paths(base):
    merge_base = subprocess.check_output(["git", "merge-base", "HEAD", base], cwd=ROOT, text=True).strip()
    diff = subprocess.check_output(
        ["git", "diff", "--name-only", "--no-renames", "-z", merge_base], cwd=ROOT, text=True)
    untracked = subprocess.check_output(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT, text=True)
    return {path for path in (diff + "\0" + untracked).split("\0") if path}


def selected_packages(base, compiler):
    packages = lock_packages(compiler)
    changed = changed_paths(base)
    if any(path.startswith(ALL_TRIGGERS) for path in changed):
        return packages
    return [member for member in packages
            if any(path.startswith(tuple(reached_directories(member, compiler))) for path in changed)]


def renamed_baseline(text, member, package_name):
    original = text
    if member in SCOPED_LAW_PACKAGES and package_name == f"hirsel.ai/{member}":
        # The first column names the test. A row can also name its owning helper
        # after '@'. Preserve the remainder of each lock line and every row.
        old = re.escape(member)
        text = re.sub(rf"(?m)^{old}(?=@[0-9]+/)|(?<=@){old}(?=@[0-9]+/)", package_name, text)
    if member == "store" and package_name == "store":
        for law in STORE_LAWS_MOVED_TO_MANIFEST:
            old = f"store@1/store/{law}"
            new = f"store@1/manifest/{law}"
            text = re.sub(rf"(?m)^{old}(?= )|(?<=@){old}(?=#)", new, text)
        old = "store@1/store/expired_cutover_keeps_old_serving"
        new = "store@1/store/test_crash_install_expired_cutover"
        text = re.sub(rf"(?m)^{old}(?= )|(?<=@){old}(?=#)", new, text)
    if text == original:
        return text
    header, *rows = text.splitlines()
    return "\n".join((header, *sorted(rows), ""))


def read_baseline(commit, member):
    path = (WORKSPACE / member / "hirsel.laws").relative_to(ROOT).as_posix()
    present = subprocess.check_output(["git", "ls-tree", commit, "--", path], cwd=ROOT)
    if not present:
        return None
    return subprocess.check_output(["git", "show", f"{commit}:{path}"], cwd=ROOT, text=True)


def moved_replay_row(commit, selected):
    # a2d3c603 moved the SRTP property to the shared receive-window package.
    # Transfer its expectation only when the recipient will run this night.
    if commit is None or not {"srtp", "replay_receive_laws"}.issubset(selected):
        return None
    source = read_baseline(commit, "srtp")
    if source is None:
        return None
    rows = [row for row in source.splitlines()[1:] if row.startswith(OLD_REPLAY_TEST + " ")]
    return rows[0] if len(rows) == 1 else None


def baseline_for(commit, member, destination, package_name, replay_row):
    if commit is None:
        return []
    text = read_baseline(commit, member)
    if replay_row and member == "srtp":
        text = text.replace(replay_row + "\n", "")
    if replay_row and member == "replay_receive_laws":
        header, *rows = ("hirsel.laws 1\n" if text is None else text).splitlines()
        moved = REPLAY_TEST + replay_row[len(OLD_REPLAY_TEST):]
        moved = moved.replace(f"@{OLD_REPLAY_TEST}#", f"@{REPLAY_TEST}#")
        text = "\n".join((header, *sorted([*rows, moved]), ""))
    if text is None:
        return []
    destination.write_text(renamed_baseline(text, member, package_name))
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


def run_package(args, member, env, commit, compiler_scratch, replay_row=None):
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
    command = [str(args.compiler), "test", *inputs,
               "--laws", "--json", "--test-timeout", "300"]
    if compiler_scratch is not None:
        command.extend(["--store", str(compiler_scratch / member)])
    manifest = hcfg.read(package / "hirsel.hcfg", "pkg_v1.Package", args.compiler)
    if hcfg.option(manifest["tests"]) is None:
        command += ["--limit", "67108864"]
    for grant in FOREIGN_GRANTS.get(member, ()):
        command += ["--grant", grant]
    if args.mode == "seed":
        command.append("--write-laws")
    else:
        baseline = baseline_for(commit, member, logs / "baseline.laws", manifest["name"], replay_row)
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
            selected = members(args.compiler)
        elif args.changed_since:
            selected = selected_packages(args.changed_since, args.compiler)
        else:
            selected = lock_packages(args.compiler)
        if args.mode == "check":
            print(f"laws check: {len(selected)} package(s)", flush=True)
        replay_row = moved_replay_row(commit, selected) if args.mode == "nightly" else None
        for member in selected:
            status |= int(run_package(args, member, env, commit, compiler_scratch, replay_row) != 0)
    return status


if __name__ == "__main__":
    sys.exit(main())
