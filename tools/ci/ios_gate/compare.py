"""Measure both counter hosts on one simulator after proving same-container relaunch."""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from test import HERE, run, simctl, simulator

EMBED = "org.hirsel.simulator-counter-embed"
PR11 = "org.hirsel.simulator-counter-pr11"


def sample(udid, bundle_id, read, start_time=None):
    container = simctl("get_app_container", udid, bundle_id, "data", capture_output=True, text=True)
    marker = Path(container.stdout.strip()) / "Documents/comparison-sample.txt"
    marker.unlink(missing_ok=True)
    launch = simctl("launch", udid, bundle_id,
                    env=dict(os.environ, SIMCTL_CHILD_HIRSEL_COMPARE_READ="1" if read else "0"),
                    capture_output=True, text=True)
    pid = int(launch.stdout.rsplit(":", 1)[1])
    try:
        deadline = time.monotonic() + 30
        while not marker.is_file():
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{bundle_id} PID {pid}: no acknowledged counter")
            time.sleep(0.05)
        counter, tick, rss = map(int, marker.read_text().split())
        assert counter == 1, f"{bundle_id}: expected counter=1, got {counter} on read={read}"
        if start_time is not None:
            elapsed = run(start_time, str(pid), str(tick), capture_output=True, text=True)
            return float(elapsed.stdout), rss
    finally:
        simctl("terminate", udid, bundle_id)


def compare(udid, arms, start_time):
    for app, bundle_id in arms:
        simctl("install", udid, app)
        sample(udid, bundle_id, read=False)
        sample(udid, bundle_id, read=True)
        simctl("uninstall", udid, bundle_id)
        print(f"{bundle_id}: acknowledged counter 1 survived same-container relaunch", flush=True)
    rows = []
    for app, bundle_id in arms:
        simctl("install", udid, app)
        rows.append(sample(udid, bundle_id, read=False, start_time=start_time))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="hirsel-counter-comparison-") as directory:
        work = Path(directory)
        (work / "CounterGate").symlink_to((args.artifacts / "CounterGate").resolve(), target_is_directory=True)
        shutil.copyfile(HERE / "counter_project.yml", work / "project.yml")
        shutil.copyfile(HERE / "CounterHost.swift", work / "CounterHost.swift")
        run("xcodegen", "generate", cwd=work)
        build = work / "derived"
        run("xcodebuild", "build", "-project", work / "HirselCounterComparison.xcodeproj",
            "-scheme", "EmbedCounter", "-configuration", "Release",
            "-destination", "generic/platform=iOS Simulator", "-derivedDataPath", build)
        embed = build / "Build/Products/Release-iphonesimulator/EmbedCounter.app"
        pr = work / "PRCounter.app"
        start_time = work / "process-start"
        run("xcrun", "--sdk", "macosx", "clang", HERE / "process_start.c", "-o", start_time)
        with tempfile.TemporaryDirectory(prefix="ref-pr11-", dir="/tmp") as checkout:
            run("bash", HERE / "build_pr.sh", checkout, pr)
        with simulator() as udid:
            embed_row, pr_row = compare(udid, [(embed, EMBED), (pr, PR11)], start_time)
        report = (
            "### iOS counter comparison\n\n"
            "Both arms acknowledged counter 1 and recovered it after relaunch.\n\n"
            "| Arm | Process start to first main-queue result | Resident bytes | Stack reserve / initial commit |\n"
            "| --- | ---: | ---: | ---: |\n"
            f"| Hirsel embed | {embed_row[0]:.3f} ms | {embed_row[1]} | 16 MiB / 64 KiB |\n"
            f"| PR #11 65a6f54 | {pr_row[0]:.3f} ms | {pr_row[1]} | 1 MiB / 256 KiB |"
        )
        print(report)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with Path(summary).open("a") as stream:
                print(report, file=stream)


if __name__ == "__main__":
    main()
