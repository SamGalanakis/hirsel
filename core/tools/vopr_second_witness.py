"""Record a Runner journal on Cranelift and check it with the LLVM release."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def prepare(compiler, runtime, work):
    work.mkdir()
    libraries = work / "libraries"
    (libraries / "programs").mkdir(parents=True)
    for name in ("core", "std"):
        (libraries / name).symlink_to(ROOT / "core" / name)
    for name in ("runner", "store"):
        shutil.copytree(ROOT / "core" / "programs" / name, libraries / "programs" / name,
                        ignore=shutil.ignore_patterns("*.md"))
    (libraries / "programs" / "hirsel.hcfg").write_text('pkg_v1.Workspace(members: ["runner", "store"])\n')
    package = libraries / "programs" / "runner"
    shutil.copyfile(ROOT / "core/tools/tapes_check_fixture.hirsel", package / "tapes_check_fixture.hirsel")
    manifest = package / "hirsel.hcfg"
    manifest.write_text(manifest.read_text().replace(
        "sources: [", 'program: Some(Program(module: Some("runner"), function: Some("witness_fixture"))), '
        'sources: [("tapes_check_fixture.hirsel", "runner"),', 1))
    for backend in ("cranelift", "llvm"):
        subprocess.run([
            compiler, "build", package, "--backend", backend, "--libraries", libraries,
            "--source-core", "--source-std", "--runtime", runtime, "--store", work / "cas",
            "--limit", "1073741824", "--dir", "store", "--grant", "Clock", "--grant", "Random",
            "--grant", "Tasks", "--grant", "Process", "--grant", "Stderr",
            "--grant", "Net=Listen 127.0.0.0/8 0-65535", "-o", work / backend,
        ], cwd=work, check=True)


def check(compiler, work, different):
    case = work / ("different" if different else "equal")
    source = case / "store"
    source.mkdir(parents=True)
    subprocess.run([work / "cranelift", "record-different" if different else "record"], cwd=case, check=True)
    before = {p.name: p.read_bytes() for p in source.iterdir()}
    checked = subprocess.run([
        compiler, "tape", "check", source, "--recorded", work / "cranelift",
        "--release", work / "llvm",
    ], cwd=case, capture_output=True, text=True)
    if checked.returncode not in (0, 1) or len(checked.stdout.splitlines()) < 2:
        raise ValueError(f"tape check exited {checked.returncode}\n{checked.stdout}{checked.stderr}")
    report = checked_report(checked.stdout, checked.returncode)
    if before != {p.name: p.read_bytes() for p in source.iterdir()}:
        raise ValueError("tape check changed the source store")
    return report


def checked_report(stdout, exit_status):
    identity, *differences, window = map(json.loads, stdout.splitlines())
    if (identity["recorded_backend"], identity["witness_backend"]) != ("Cranelift", "Llvm"):
        raise ValueError(f"unexpected witness pair: {identity}")
    count = window["different"]
    if len(differences) != count or exit_status != int(count != 0):
        raise ValueError(f"tape check exit or differences disagree with its report: {window}")
    if count:
        status = "different"
    elif not window["same"] or window["same"] + window["not_recorded"] != window["through"] - window["after"]:
        status = "inconclusive"
    else:
        status = "clean"
    return {"status": status, "images": identity, "window": window, "differences": differences}


def expect_case(report, different):
    window = report["window"]
    if (window["after"], window["through"], window["same"], window["different"],
            window["not_recorded"]) != (0, 2, int(not different), int(different), 1):
        raise ValueError(f"unexpected replay counts: {window}")
    expected = [{"version": 2, "difference": "Reply"}] if different else []
    if report["differences"] != expected:
        raise ValueError(f"unexpected replay differences: {report['differences']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("compiler", type=Path)
    parser.add_argument("runtime", type=Path)
    parser.add_argument("work", type=Path)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--law", action="store_true", help="also check a deliberately different reply")
    args = parser.parse_args()
    compiler, runtime, work = (path.resolve() for path in (args.compiler, args.runtime, args.work))
    try:
        prepare(compiler, runtime, work)
        result = check(compiler, work, False)
        if args.law:
            expect_case(result, False)
            divergent = check(compiler, work, True)
            expect_case(divergent, True)
            result["law_different"] = divergent
    except (OSError, KeyError, ValueError, subprocess.CalledProcessError) as error:
        result = {"status": "failed", "reason": str(error)}
        print(f"second witness: {error}", file=sys.stderr)
    args.receipt.write_text(json.dumps(result) + "\n")
    return int(result["status"] in ("different", "failed"))


if __name__ == "__main__":
    sys.exit(main())
