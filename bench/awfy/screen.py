#!/usr/bin/env python3
"""Screen matched pairs of Hirsel board binaries for instructions, size and peak RSS."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROWS = (("awfy", "Bounce"), ("pool", "List"), ("arena", "CLBG::Nbody"))
BOAT_RUN = Path("/workspace/code/hirsel-briefs/tools/boat-dispatch")
MEMORY_CAP = ("systemd-run", "--user", "--scope", "--quiet",
              "-p", "MemoryMax=2G", "-p", "MemorySwapMax=0")
ARMS = ("base", "candidate")


def workload(value):
    suite, slash, name = value.partition("/")
    if not slash or suite not in ("awfy", "pool", "arena") or not name:
        raise argparse.ArgumentTypeError("expected an awfy, pool or arena row as suite/name")
    return suite, name


def check_peer(checkout, name):
    import board

    if (board.git(checkout, "rev-parse", "HEAD") != board.PINS[name] or
            board.git(checkout, "status", "--porcelain", "--untracked-files=no")):
        raise ValueError(f"{checkout}: expected clean {name} at {board.PINS[name]}")


def prepare(args):
    import arena
    import board
    import build

    if args.out.is_relative_to(build.ROOT) or board.git(build.ROOT, "status", "--porcelain"):
        raise ValueError("prepare from a clean checkout into a new directory outside the checkout")
    rows = args.row or ROWS
    entries = {("awfy", False): "harness", ("pool", False): "pool_index", ("pool", True): "pools"}
    awfy_entries = sorted({entries[suite, name in ("JsonPool", "StoragePool")]
                           for suite, name in rows if suite != "arena"})
    arena_rows = [{"name": name} for suite, name in rows if suite == "arena"]
    if awfy_entries:
        check_peer(args.awfy, "awfy")
    if arena_rows:
        check_peer(args.langarena, "langarena")
    args.out.mkdir()
    env = build.capped_env(args.out, "8G", "2")
    artifact_file = args.out / "toolchain.json"
    artifacts, _ = board.prepare_toolchain(artifact_file, env, jobs=2)
    if awfy_entries:
        awfy = args.out / "awfy"
        awfy.mkdir()
        build.hirsel(argparse.Namespace(out=awfy, artifacts=artifact_file, upstream=args.awfy,
                                       env=env, hirsel_entries=awfy_entries), matched=True)
    if arena_rows:
        arena_dir = args.out / "arena"
        arena_dir.mkdir()
        config = arena_dir / "run.js"
        shutil.copyfile(args.langarena / config.name, config)
        arena.build_hirsel(arena_dir, artifacts, arena_rows, env, directory_grant=".")
    commit = board.git(build.ROOT, "rev-parse", "HEAD")
    (args.out / "screen.json").write_text(json.dumps({"commit": commit}) + "\n")


def stage(directory, upstream, target, selected, *, instructions_only):
    import awfy_measure
    import build
    from elf_census import strip_image

    metadata = directory / "screen.json"
    commit = json.loads(metadata.read_text())["commit"]
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError(f"{metadata}: missing build commit")
    target.mkdir()
    config = directory / "arena/run.js"
    if any(suite == "arena" for suite, _ in selected):
        shutil.copyfile(config, target / config.name)
    rows = []
    for suite, name in selected:
        cases = (awfy_measure.arena_rows(config.parent, config) if suite == "arena"
                 else awfy_measure.runtime_cases(suite, directory / "awfy", upstream))
        binary_and_args, marker = dict(cases)[name]["hirsel"]
        binary, *arguments = binary_and_args
        report = json.loads(binary.with_name(f"{binary.name}-build.json").read_text())
        if (report.get("accepted") is not True or report.get("stopped") or
                not report["native"]["linked"] or
                report["measurement"]["image_sha256"] != build.sha256(binary)):
            raise ValueError(f"{binary}: expected the accepted, linked board image")
        shipped = target / binary.name
        if not shipped.exists():
            strip_image(binary, shipped)
            shipped.chmod(0o755)
        row = {"name": f"{suite}/{name}", "argv": [f"./{shipped.name}", *arguments],
               "marker": marker.decode(),
               "workload_sources_sha256": report["measurement"]["workload_sources_sha256"]}
        if instructions_only:
            row.update({field: report["measurement"][field] for field in ("flags", "clang_sha256")})
        rows.append(row)
    return commit, rows


def screen_job(args, bundle, selected=ROWS, *, instructions_only=False):
    if any(suite != "arena" for suite, _ in selected):
        check_peer(args.awfy, "awfy")
    commits = {}
    row_sets = []
    for arm, source in zip(ARMS, (args.base, args.candidate), strict=True):
        commits[arm], rows = stage(source, args.awfy, bundle / arm, selected,
                                   instructions_only=instructions_only)
        row_sets.append(rows)
    if (row_sets[0] != row_sets[1] or
            (any(suite == "arena" for suite, _ in selected) and
             (bundle / "base/run.js").read_bytes() != (bundle / "candidate/run.js").read_bytes())):
        raise ValueError("base and candidate board workload contracts differ")
    shutil.copyfile(__file__, bundle / "screen.py")
    (bundle / "inputs.json").write_text(json.dumps({"commits": commits, "rows": row_sets[0]}))
    command = "python3 screen.py sample"
    if instructions_only:
        command += " --instructions-only > instructions.tsv"
    return {"lane": "board-screen", "bundle": str(bundle), "command": command, "seconds": 30}


def run_screen(args):
    args.out.mkdir()
    with tempfile.TemporaryDirectory(dir=args.out) as directory:
        bundle = Path(directory)
        job = screen_job(args, bundle)
        subprocess.run([args.boat_run, "--log-dir", args.out / "boat-logs",
                        job["lane"], job["bundle"], job["command"]],
                       check=True, cwd=args.out)


def checked_sample(command, directory, marker):
    result = subprocess.run([*MEMORY_CAP, *command], cwd=directory,
                            stdout=subprocess.PIPE, check=True)
    if not any(re.fullmatch(marker, line) for line in result.stdout.splitlines(keepends=True)):
        raise ValueError(f"{directory.name}: {command[-1]} output failed verification: {result.stdout!r}")


def sample(*, instructions_only=False):
    data = json.loads(Path("inputs.json").read_text())
    header = "workload\tarm\tcommit\tinstructions"
    print(header if instructions_only else header + "\tstripped_bytes\tpeak_rss_kib", flush=True)
    for row in data["rows"]:
        for arm in ARMS:
            directory = Path(arm)
            argv, marker = row["argv"], row["marker"].encode()
            checked_sample(["sudo", "-n", "perf", "stat", "--no-big-num", "-x", ",",
                            "-e", "instructions:u", "-o", "instructions.csv", "--", *argv],
                           directory, marker)
            fields, = [line.split(",") for line in (directory / "instructions.csv").read_text().splitlines()
                       if not line.startswith("#") and ",instructions:u," in line]
            count = int(fields[0])
            if count <= 0 or float(fields[4]) != 100:
                raise ValueError(f"{arm}/{row['name']}: instruction counter was not fully active")
            if instructions_only:
                print(row["name"], arm, data["commits"][arm], count, sep="\t", flush=True)
                continue
            checked_sample(["/usr/bin/time", "-f", "%M", "-o", "rss.txt", *argv], directory, marker)
            rss = int((directory / "rss.txt").read_text())
            if rss <= 0:
                raise ValueError(f"{arm}/{row['name']}: no peak RSS sample")
            size = (directory / argv[0]).stat().st_size
            print(row["name"], arm, data["commits"][arm], count, size, rss, sep="\t", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    local = commands.add_parser("prepare", help="build the selected images locally from this checkout")
    local.add_argument("out", type=lambda value: Path(value).resolve())
    local.add_argument("--awfy", type=Path, required=True)
    local.add_argument("--langarena", type=Path, required=True)
    local.add_argument("--row", type=workload, action="append", help="build a selected suite/name")
    pair = argparse.ArgumentParser(add_help=False)
    pair.add_argument("out", type=lambda value: Path(value).resolve())
    pair.add_argument("--base", type=Path, required=True, help="directory from screen.py prepare")
    pair.add_argument("--candidate", type=Path, required=True, help="directory from screen.py prepare")
    pair.add_argument("--awfy", type=Path, required=True)
    boat = commands.add_parser("run", parents=[pair], help="compare two built directories in one Boat VM")
    commands.add_parser("manifest", parents=[pair], help="stage a board screen shard for Boat")
    launcher = BOAT_RUN if BOAT_RUN.is_file() else BOAT_RUN.with_name("boat-run.sh")
    boat.add_argument("--boat-run", type=lambda value: Path(value).resolve(), default=launcher)
    remote = commands.add_parser("sample", help="measure a staged bundle inside its Boat VM")
    remote.add_argument("--instructions-only", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args)
        elif args.command == "run":
            run_screen(args)
        elif args.command == "manifest":
            args.out.mkdir()
            print(json.dumps([screen_job(args, args.out)]))
        else:
            sample(instructions_only=args.instructions_only)
    except (OSError, KeyError, ValueError, subprocess.CalledProcessError) as error:
        print(f"board screen: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
