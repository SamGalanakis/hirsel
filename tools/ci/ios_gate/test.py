"""Run the generated package in a hosted XCTest on macOS and a fresh iOS simulator."""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from tls_fixture import serve

HERE = Path(__file__).resolve().parent


def run(*command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def simctl(*args, **kwargs):
    return run("xcrun", "simctl", *args, **kwargs)


@contextmanager
def simulator(cert=None):
    simctl("list", "runtimes")
    runtimes = json.loads(simctl("list", "runtimes", "--json", capture_output=True, text=True).stdout)
    ios = [row for row in runtimes["runtimes"]
           if row["isAvailable"] and row["identifier"].startswith("com.apple.CoreSimulator.SimRuntime.iOS-")
           and tuple(int(n) for n in row["version"].split(".")) >= (26, 0)]
    runtime = max(ios, key=lambda row: tuple(int(n) for n in row["version"].split(".")))
    devices = json.loads(simctl("list", "devicetypes", "--json", capture_output=True, text=True).stdout)
    phones = [device for device in devices["devicetypes"] if device["productFamily"] == "iPhone"]
    # Prefer the newest phone supported by the installed runtime.
    version = [int(n) for n in runtime["version"].split(".")]
    major, minor, patch = (version + [0, 0])[:3]
    encoded = (major << 16) | (minor << 8) | patch
    device = next(row for row in phones if row["minRuntimeVersion"] <= encoded <= row["maxRuntimeVersion"])
    udid = simctl("create", "Hirsel-CI", device["identifier"], runtime["identifier"],
                  capture_output=True, text=True).stdout.strip()
    try:
        print(f"Simulator {udid} pinned to {runtime['identifier']}, {device['identifier']}", flush=True)
        simctl("boot", udid)
        simctl("bootstatus", udid, "-b")
        if cert is not None:
            simctl("keychain", udid, "add-root-cert", cert)
        yield udid
    finally:
        simctl("delete", udid)


def test(work, logs, name, destination):
    result = logs / f"{name}.xcresult"
    logfile = logs / f"{name}.log"
    command = [
        "xcodebuild", "test", "-project", work / "HirselSimulatorGate.xcodeproj",
        "-scheme", f"Gate-{name}", "-destination", destination, "-parallel-testing-enabled", "NO",
        "-derivedDataPath", work / f"build-{name}", "-resultBundlePath", result,
    ]
    with subprocess.Popen(command, cwd=work, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True, errors="replace") as process, logfile.open("w") as stream:
        for line in process.stdout:
            print(line, end="", flush=True)
            stream.write(line)
        status = process.wait()

    measurements = [line.split("HIRSEL_MEASUREMENT ", 1)[1].strip()
                    for line in logfile.read_text().splitlines() if "HIRSEL_MEASUREMENT " in line]
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with Path(summary).open("a") as stream:
            print(f"### {name}\n", file=stream)
            if measurements:
                for line in measurements:
                    print(f"- `{line}`", file=stream)
            else:
                print("No measurement samples emitted.", file=stream)
    return status == 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("logs", type=Path)
    args = parser.parse_args()
    logs = args.logs.resolve()
    logs.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-xctest-") as directory:
        work = Path(directory)
        for name in ("project.yml", "Host.swift", "Tests.swift", "AbiTests.swift", "AbiTests.h",
                     "Workers.c", "Memory.h", "TLSTests.swift"):
            shutil.copyfile(HERE / name, work / name)
        (work / "SimGate").symlink_to((args.artifacts / "SimGate").resolve(), target_is_directory=True)
        resources = work / "TLS"
        resources.mkdir()
        certs = work / "certificates"
        certs.mkdir()
        with serve(certs, resources) as cert:
            run("xcodegen", "generate", cwd=work)
            mac = test(work, logs, "macOS", "platform=macOS,arch=arm64")
            with simulator(cert) as udid:
                ios = test(work, logs, "iOS", f"platform=iOS Simulator,id={udid}")
    return 0 if mac and ios else 1


if __name__ == "__main__":
    raise SystemExit(main())
