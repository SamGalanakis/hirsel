#!/usr/bin/env python3
"""Build the Linux installation, then export only inputs the Mac gate consumes."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BUCK = ROOT / ".buck2/bin/buck2"
DRIVER = "root//:hirsel-core-driver-bin-hirsel"
MAC_RUNTIME = "root//:hirsel-core-rt-archives[aarch64-apple-darwin]"
MAC_EMBED = "root//:hirsel-core-rt-embed-archives[aarch64-apple-darwin]"
IOS_EMBED = "root//:hirsel-core-rt-ios-embed-archives"
KIT = "root//tools/buck2/native-toolchain:kit"
PACKAGE = """[package]
name = "sim_gate"
major = 1
charge = "charge-v1"

[dependencies]
std = { major = 1 }
store = { major = 1, path = "../store" }
runner = { major = 1, path = "../runner" }

[sources]
"fixture.hirsel" = "main"
"counter.hirsel" = "counter"
"""


def export(compiler, kit, output):
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-swift-inputs-") as directory:
        work = Path(directory)
        fixture = (HERE / "fixture.hirsel").read_text()
        alternate = fixture.replace("n + 7u64", "n + 8u64")
        assert fixture != alternate
        for package, source in (("SimGate", fixture), ("SimGateAlternate", alternate)):
            workspace = work / package
            libraries = workspace / "libraries"
            programs = libraries / "programs"
            programs.mkdir(parents=True)
            (workspace / "hirsel.toml").write_text(
                '[workspace]\nmembers = ["store", "runner", "app"]\n'
            )
            for name in ("core", "std"):
                (libraries / name).symlink_to(ROOT / "core" / name, target_is_directory=True)
            for name in ("runner", "store"):
                staged = workspace / name
                shutil.copytree(ROOT / "core/programs" / name, staged)
                (programs / name).symlink_to(staged, target_is_directory=True)

            app = workspace / "app"
            app.mkdir()
            (app / "hirsel.toml").write_text(PACKAGE)
            (app / "fixture.hirsel").write_text(source)
            shutil.copyfile(HERE / "counter.hirsel", app / "counter.hirsel")
            subprocess.run(
                [compiler, "sdk", "swift", app, "--libraries", libraries,
                 "--store", work / "compiler-store", "--limit", "1073741824",
                 "-o", output / package],
                check=True,
            )

    for triple in ("aarch64-apple-ios", "aarch64-apple-ios-sim"):
        subprocess.run(
            ["python3", ROOT / "tools/buck2/native-toolchain/link_kit.py", "symbol-fixture",
             "--llvm", ROOT / "tools/buck2/toolchains/llvm-files", "--kit", kit,
             "--target", triple, "--output", output / f"{triple}.o"],
            check=True,
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    built = json.loads(subprocess.check_output(
        [BUCK, "build", "-j", str(args.jobs), "--show-json-output",
         DRIVER, KIT, MAC_RUNTIME, MAC_EMBED, IOS_EMBED],
        cwd=ROOT, text=True,
    ))
    kit = ROOT / built[KIT]
    with tempfile.TemporaryDirectory(prefix="hirsel-swift-installation-") as directory:
        prefix = Path(directory) / "prefix"
        install = [ROOT / built[DRIVER], "core-product", "--install", prefix, "--link-kit", kit]
        subprocess.run(
            [*install, "--target", "aarch64-apple-darwin", "--runtime", ROOT / built[MAC_RUNTIME],
             "--embed-runtime", ROOT / built[MAC_EMBED]],
            cwd=ROOT, check=True,
        )
        for triple in ("aarch64-apple-ios", "aarch64-apple-ios-sim"):
            archive = ROOT / built[IOS_EMBED] / triple / "libhirsel_core_rt.a"
            subprocess.run([*install, "--target", triple, "--embed-runtime", archive],
                           cwd=ROOT, check=True)
        export(prefix / "hirsel", kit, args.output.resolve())


if __name__ == "__main__":
    main()
