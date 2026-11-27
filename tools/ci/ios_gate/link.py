"""Qualify the generated XCFramework and archive links against Xcode."""

import argparse
import json
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
FRAMEWORK = "SimGateCore.xcframework"
FIELDS = (
    "LibraryIdentifier", "LibraryPath", "HeadersPath", "SupportedArchitectures",
    "SupportedPlatform", "SupportedPlatformVariant",
)


def run(*command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def libraries(bundle):
    with (bundle / "Info.plist").open("rb") as stream:
        rows = plistlib.load(stream)["AvailableLibraries"]
    assert len(rows) == 3
    return sorted(tuple(row.get(key) for key in FIELDS) for row in rows)


def check_plist(bundle, scratch):
    rebuilt = scratch / FRAMEWORK
    command = ["xcodebuild", "-create-xcframework"]
    for identifier, archive, headers, *_ in libraries(bundle):
        command.extend(["-library", bundle / identifier / archive,
                        "-headers", bundle / identifier / headers])
    run(*command, "-output", rebuilt)
    assert libraries(rebuilt) == libraries(bundle), (libraries(rebuilt), libraries(bundle))
    print("XCFramework rows agree with xcodebuild", flush=True)


def sdk_path(sdk):
    return run("xcrun", "--sdk", sdk, "--show-sdk-path", capture_output=True, text=True).stdout.strip()


def check_symbols(artifacts, scratch):
    targets = json.loads((ROOT / "tools/native-toolchain/apple-targets.json").read_text())["targets"]
    for target, sdk in (("ios", "iphoneos"), ("ios-simulator", "iphonesimulator")):
        row = targets[target]
        triple = f"arm64-apple-ios{row['floor']}"
        if target == "ios-simulator":
            triple += "-simulator"
        run("xcrun", "--sdk", sdk, "clang", "-target", triple, "-isysroot", sdk_path(sdk),
            "-dynamiclib", artifacts / f"{row['triple']}.o",
            "-framework", "CoreFoundation", "-framework", "Security",
            "-o", scratch / f"{target}.dylib")
        print(f"All {target} symbol references linked against the SDK", flush=True)


def check_duplicate(artifacts, scratch):
    packages = [artifacts / name for name in ("SimGate", "SimGateAlternate")]
    assert (packages[0] / "projection.json").read_bytes() == (packages[1] / "projection.json").read_bytes()
    target = "arm64-apple-ios26.0-simulator"
    swiftc = ["xcrun", "--sdk", "iphonesimulator", "swiftc",
              "-target", target, "-sdk", sdk_path("iphonesimulator"), "-O"]
    objects, archives, anchors = [], [], []
    for suffix, package in zip(("A", "B"), packages, strict=True):
        slice_dir = package / FRAMEWORK / "ios-arm64-simulator"
        header = slice_dir / "Headers/hirsel_product.h"
        anchors.append(re.search(r"require_anchor_([a-f0-9]{16})", header.read_text())[1])
        wrapper = scratch / f"{suffix}.swift"
        wrapper.write_text(
            f'@_cdecl("start{suffix}") public func start{suffix}() {{\n'
            '    _ = try? Runtime.start(grants: "")\n}\n'
        )
        obj = scratch / f"{suffix}.o"
        run(*swiftc, "-whole-module-optimization", "-parse-as-library", "-emit-object",
            "-module-name", f"Optimized{suffix}", "-I", slice_dir / "Headers",
            *sorted((package / "Sources/SimGate").glob("*.swift")), wrapper, "-o", obj)
        objects.append(obj)
        archives.append(slice_dir / "libsim_gate.a")
    assert anchors[0] != anchors[1], "comparison archives must have distinct anchors"
    main = scratch / "main.swift"
    main.write_text(
        '@_silgen_name("startA") func startA()\n'
        '@_silgen_name("startB") func startB()\nstartA()\nstartB()\n'
    )
    for order in (archives, archives[::-1]):
        result = subprocess.run(
            [*swiftc, main, *objects, *order, "-framework", "CoreFoundation",
             "-framework", "Security", "-o", scratch / "duplicate"],
            capture_output=True, text=True,
        )
        print(result.stderr, end="", flush=True)
        assert result.returncode != 0, f"both archives linked successfully: {order}"
        assert re.search(r"duplicate symbol[^\n]*_hirsel_embed_v1_", result.stderr), order


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve()
    with tempfile.TemporaryDirectory(prefix="hirsel-ios-link-") as root:
        scratch = Path(root)
        check_plist(artifacts / "SimGate" / FRAMEWORK, scratch)
        check_symbols(artifacts, scratch)
        check_duplicate(artifacts, scratch)


if __name__ == "__main__":
    main()
