"""Check the iOS rows as part of the native toolchain proof."""

import json
from pathlib import Path
import re
import subprocess
import sys

from link_kit import symbol_names, tbd_exports


def undefined(nm, obj):
    command = [nm, "-u", "--no-demangle", "--format=posix", obj]
    return {line.split()[0] for line in subprocess.check_output(command, text=True).splitlines()
            if line and not line.endswith(":")}


def same_symbols(label, actual, expected):
    if actual != expected:
        raise SystemExit(f"{label}: missing {sorted(expected - actual)}, extra {sorted(actual - expected)}")


def removed_system_name(name):
    # EMB-7: no Exec, standalone executable path or listener inspection, or
    # directory-relative Unix naming. PF_ROUTE has no dedicated Mach-O symbol.
    return name.startswith("_posix_spawn") or name in {
        "__NSGetExecutablePath", "_fork", "_vfork", "_kill", "_waitpid",
        "_proc_pidfdinfo", "_proc_pidinfo", "_pthread_fchdir_np",
        "_socketpair",  # The embedded Exec helper is desktop-only.
    }


def prove_runtime(llvm, runtime, target, lists):
    nm = llvm / "bin/llvm-nm"
    definitions = subprocess.check_output([nm, "--defined-only", "--no-demangle", "--format=posix", runtime], text=True)
    for symbol in ("CRYPTO_memcmp", "ChaCha20_ctr32_neon"):
        pattern = rf"^_ring_core_\d+_\d+_\d+__{symbol} [Tt] "
        if not re.search(pattern, definitions, re.M):
            raise SystemExit(f"{runtime}: missing native ring definition {symbol}")

    imports = undefined(nm, runtime)
    for prefix, library in ((("_CF", "_kCF"), "CoreFoundation"), (("_Sec",), "Security")):
        framework_imports = {name for name in imports if name.startswith(prefix)}
        if not framework_imports:
            raise SystemExit(f"{runtime}: no {library} verifier imports")
        same_symbols(f"{runtime} {library} verifier import coverage", framework_imports - lists[library], set())

    tool = llvm / "bin/llvm-readobj"
    headers = subprocess.check_output([tool, "--file-headers", "--macho-version-min", runtime], text=True)
    objects = headers.split("\nFile: ")[1:]
    if not objects:
        raise SystemExit(f"{runtime}: no Mach-O objects")
    platform = target["platform_version_name"].replace("-", "")
    floor = tuple(map(int, target["floor"].split("."))) + (0,)
    for header in objects:
        member = header.splitlines()[0]
        versions = re.findall(r"Cmd: LC_BUILD_VERSION\s+Size: \d+\s+Platform: (\w+)\s+Version: ([\d.]+)", header)
        # The pinned device std was built for iOS 10. Its legacy command
        # identifies the device on arm64. A simulator always has platform 7.
        if platform == "ios":
            versions.extend(("ios", version) for version in re.findall(
                r"Cmd: LC_VERSION_MIN_IPHONEOS\s+Size: \d+\s+Version: ([\d.]+)", header))
        if ("Format: Mach-O arm64\n" not in header or "FileType: Relocatable" not in header
                or len(versions) != 1 or versions[0][0] != platform):
            raise SystemExit(f"{member}: expected one relocatable arm64 {platform} build version")
        version = tuple(map(int, versions[0][1].split(".")))
        if version > floor:
            raise SystemExit(f"{member}: minimum {versions[0][1]} exceeds {target['floor']}")
    print(f"{target['triple']}: checked {len(objects)} runtime members, ring C and assembly, and verifier imports", flush=True)


def main():
    llvm, kit, work, generator, records, runtimes, macos, system, cf, security = map(Path, sys.argv[1:])
    targets = json.loads(records.read_text())
    manifest = json.loads((kit / "manifest.json").read_text())["targets"]
    lists = {library: set(symbol_names(path)) for library, path in
             (("libSystem", system), ("CoreFoundation", cf), ("Security", security))}
    names = set.union(*lists.values())
    same_symbols("iOS libSystem", lists["libSystem"],
                 {name for name in symbol_names(macos) if not removed_system_name(name)})
    banned = {name for name in names if removed_system_name(name) or name in {
        "_SecTrustSettingsCopyCertificates", "_SecTrustSettingsCopyTrustSettings",
        "_SecTrustSetAnchorCertificates", "_SecTrustSetAnchorCertificatesOnly",
    }}
    same_symbols("iOS exclusions", banned, set())

    nm = llvm / "bin/llvm-nm"
    for target in targets:
        triple = target["triple"]
        prove_runtime(llvm, runtimes / triple / "libhirsel_core_rt.a", target, lists)
        row = manifest[triple]
        for field in ("floor", "llvm_triple", "platform_version_name"):
            if row[field] != target[field]:
                raise SystemExit(f"{triple}: manifest {field} differs from its Apple target record")
        same_symbols(f"{triple} stub names", set(row["libraries"]), {name + ".tbd" for name in lists})
        platform = row["platform_version_name"]
        stubs = [kit / triple / name for name in row["libraries"]]
        for stub in stubs:
            same_symbols(f"{triple} {stub.name} exports", tbd_exports(stub.read_text(), platform), lists[stub.stem])
        fixture = work / f"{triple}.o"
        subprocess.run(["python3", generator, "symbol-fixture", "--llvm", llvm, "--kit", kit,
                        "--target", triple, "--output", fixture], check=True)
        same_symbols(f"{triple} fixture imports", undefined(nm, fixture), names)
        dylib = fixture.with_suffix(".dylib")
        floor = row["floor"]
        subprocess.run([kit / "ld.lld", "-flavor", "darwin", "-dylib", "-arch", "arm64", "-platform_version",
                        platform, floor, floor, "-dead_strip", fixture, *stubs, "-o", dylib], check=True)
        same_symbols(f"{triple} linked fixture imports", undefined(nm, dylib), names)
        print(f"{triple}: all {len(names)} fixture references linked against the kit at {floor}", flush=True)


if __name__ == "__main__":
    main()
