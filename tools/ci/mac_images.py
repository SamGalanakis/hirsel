#!/usr/bin/env python3
"""Export the driver's Mach-O law images on Linux and verify and run them on Mac."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

BUCK = Path(".buck2/bin/buck2")
HARNESS = "root//:hirsel-core-driver-unittest-harness"
KIT = "root//tools/buck2/native-toolchain:kit"
RUNTIME = "root//:core-cranelift-runtime-fixtures[aarch64-apple-darwin]"
ARCHIVE = "libcore_cranelift_fixture.a"
LAW_COUNTS = {
    "a_patched_macho_link_is_the_lld_link_of_the_edited_objects": 5,
    "an_edited_weak_copy_is_not_patched_over_the_copy_lld_kept": 1,
    "an_edited_weak_copy_under_a_strong_definition_falls_back": 4,
    "an_edited_static_patches_a_macho_image": 1,
    "a_function_body_edit_patches_a_macho_image": 3,
}


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def images(root):
    result = []
    for law, count in LAW_COUNTS.items():
        paths = sorted((root / law).glob("*.macho"))
        if len(paths) != count:
            raise SystemExit(f"{law}: expected {count} images, found {len(paths)}")
        for path in paths:
            with path.open("rb") as stream:
                header = stream.read(32)
            if len(header) != 32:
                raise SystemExit(f"{path}: incomplete Mach-O header")
            magic, cpu, _, kind = struct.unpack_from("<IiiI", header)
            if (magic, cpu, kind) != (0xFEEDFACF, 0x0100000C, 2):
                raise SystemExit(f"{path}: expected an arm64 Mach-O executable")
            result.append(path)
    return result


def export(root):
    root.mkdir(parents=True)
    built = json.loads(subprocess.check_output(
        [BUCK, "build", "-j4", "--show-json-output", HARNESS, KIT, RUNTIME], text=True,
    ))
    prefix = Path("tools/buck2/toolchains/llvm-files").resolve()
    env = dict(os.environ, LLVM_SYS_231_PREFIX=str(prefix),
               HIRSEL_LINK_KIT=str(Path(built[KIT]).resolve()), HIRSEL_EXPORT_DARWIN_IMAGES=str(root))
    subprocess.run([built[HARNESS], "incremental_link::tests::macho::"], check=True, env=env)
    paths = [*images(root), root / ARCHIVE]
    shutil.copyfile(built[RUNTIME], root / ARCHIVE)
    manifest = {path.relative_to(root).as_posix(): sha256(path) for path in paths}
    (root / "sha256.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def verify(root):
    paths = [*images(root), root / ARCHIVE]
    actual = {path.relative_to(root).as_posix(): sha256(path) for path in paths}
    expected = json.loads((root / "sha256.json").read_text())
    if actual != expected:
        mismatch = sorted(name for name in actual.keys() | expected.keys()
                          if actual.get(name) != expected.get(name))
        raise SystemExit(f"Linux artifact digest mismatch: {mismatch}")
    print(f"Verified {len(paths) - 1} Linux image digests and the native C fixture archive")


def run(root):
    failed = set()
    for source in images(root):
        law = source.parent.name
        name, status = source.stem.rsplit(".", 1)
        with tempfile.TemporaryDirectory(prefix="hirsel-mac-law-") as scratch:
            work = Path(scratch)
            image = work / name
            shutil.copyfile(source, image)
            image.chmod(0o755)
            try:
                subprocess.run(["/usr/bin/codesign", "--verify", "--strict", image], check=True)
                actual = subprocess.run([image], cwd=work, timeout=300).returncode
                if actual != int(status):
                    raise ValueError(f"expected exit {status}, saw {actual}")
            except (subprocess.SubprocessError, ValueError, OSError) as error:
                failed.add(law)
                print(f"{law}/{name}: {error}", flush=True)
            else:
                print(f"{law}/{name}: exit {actual}", flush=True)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        print(f"failures={json.dumps(sorted(failed))}", file=output)
    return bool(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("export", "verify", "run"))
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    match args.command:
        case "export":
            export(root)
        case "verify":
            verify(root)
        case "run":
            return run(root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
