"""Check the iOS rows as part of the native toolchain proof."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from link_kit import symbol_names, tbd_exports


def undefined(nm, obj):
    command = [nm, "-u", "--no-demangle", "--format=posix", obj]
    return {line.split()[0] for line in subprocess.check_output(command, text=True).splitlines() if line}


def same_symbols(label, actual, expected):
    if actual != expected:
        raise SystemExit(f"{label}: missing {sorted(expected - actual)}, extra {sorted(actual - expected)}")


def removed_system_name(name):
    # EMB-7: no Exec, standalone executable path or listener inspection, or
    # directory-relative Unix naming. PF_ROUTE has no dedicated Mach-O symbol.
    return name.startswith("_posix_spawn") or name in {
        "__NSGetExecutablePath", "_fork", "_vfork", "_kill", "_waitpid",
        "_proc_pidfdinfo", "_proc_pidinfo", "_pthread_fchdir_np",
    }


def main():
    llvm, kit, work, generator, records, rust, verifier, probe, macos, system, cf, security = map(Path, sys.argv[1:])
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
    device = next(row for row in targets if not row["llvm_environment"])
    shutil.copyfile(verifier, work / "apple.rs")
    shutil.copyfile(probe, work / "ios_probe.rs")
    obj = work / "ios_probe.o"
    env = dict(os.environ, **{device["deployment_env"]: device["floor"]})
    subprocess.run([rust / "bin/rustc", "--target", device["triple"], "--edition=2024", "--crate-type=lib",
                    "--emit=obj", "-Cpanic=abort", "-Csymbol-mangling-version=v0", "-Dwarnings",
                    work / "ios_probe.rs", "-o", obj], check=True, env=env)
    imports = {name for name in undefined(nm, obj) if not name.startswith("__R")}
    for prefix, library in ((("_CF", "_kCF"), "CoreFoundation"), ("_Sec", "Security")):
        framework_imports = {name for name in imports if name.startswith(prefix)}
        if not framework_imports:
            raise SystemExit(f"iOS verifier object has no {library} imports")
        same_symbols(f"{library} verifier import coverage", framework_imports - lists[library], set())
    same_symbols("iOS verifier import coverage", imports - names, set())
    print(f"{device['triple']}: {len(imports)} verifier imports covered by the iOS lists", flush=True)

    for target in targets:
        triple = target["triple"]
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
