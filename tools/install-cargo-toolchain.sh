#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s [--prefix DIRECTORY]\n' "$0"
}

case "$#" in
    0) prefix="$HOME/.local/share/hirsel-cargo" ;;
    1)
        if [[ "$1" == --help || "$1" == -h ]]; then
            usage
            exit 0
        fi
        usage >&2
        exit 2
        ;;
    2)
        if [[ "$1" != --prefix || -z "$2" ]]; then
            usage >&2
            exit 2
        fi
        prefix="$2"
        ;;
    *) usage >&2; exit 2 ;;
esac

case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
        target=x86_64-unknown-linux-gnu
        runtime_profile=release
        runtime_flags=(--locked)
        # The Linux publisher rewrites DT_RPATH when it copies the compiler.
        driver_flags=(-- -C link-arg=-Wl,--disable-new-dtags -C 'link-arg=-Wl,-rpath,$ORIGIN/lib:$ORIGIN/../lib')
        ;;
    Darwin:arm64)
        target=aarch64-apple-darwin
        # Use the shipped runtime profile to remove unused framework references.
        runtime_profile=dist
        runtime_flags=(--locked --config 'profile.dist.panic="abort"')
        driver_flags=(--)
        ;;
    *)
        printf 'Cargo installation supports Linux x86_64 and macOS arm64.\n' >&2
        exit 2
        ;;
esac

prefix=$(python3 - "$prefix" <<'PY'
from pathlib import Path
import sys

requested = Path(sys.argv[1])
prefix = requested.parent.resolve() / requested.name
if prefix.is_symlink() or (prefix.exists() and (not prefix.is_dir() or any(prefix.iterdir()))):
    raise SystemExit(f"installation prefix must be empty or new: {prefix}")
prefix.parent.mkdir(parents=True, exist_ok=True)
print(prefix)
PY
)

cd "$(dirname "$0")/.."
repo=$(pwd -P)
export LLVM_SYS_231_PREFIX="$repo/tools/buck2/toolchains/llvm-files"
if [[ "$target" == aarch64-apple-darwin ]]; then
    MACOSX_DEPLOYMENT_TARGET=$(python3 -B tools/buck2/apple_records.py macos floor)
    export MACOSX_DEPLOYMENT_TARGET
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/hirsel-cargo.XXXXXX")
stage=
cleanup() {
    rm -rf -- "$work"
    if [[ -n "$stage" ]]; then
        rm -rf -- "$stage"
    fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
stage=$(mktemp -d "$(dirname "$prefix")/.hirsel-cargo.XXXXXX")

python3 -B tools/buck2/bootstrap_llvm.py
target_dir=$(cargo metadata --locked --no-deps --format-version 1 |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
cargo rustc --locked --release --target "$target" -p hirsel-core-driver --bin hirsel "${driver_flags[@]}"
cargo rustc "${runtime_flags[@]}" --profile "$runtime_profile" --target "$target" -p hirsel-core-rt --lib --features standalone \
    --crate-type staticlib -- -C relocation-model=pic

# Use the distribution's pinned link inputs and the compiler's normal installer.
python3 -B - "$work" "$target" <<'PY'
from pathlib import Path
import json
import os
import subprocess
import sys
import tarfile

work = Path(sys.argv[1])
if sys.argv[2] == "aarch64-apple-darwin":
    sys.path.insert(0, "tools/toolchain-dist")
    from darwin_host import link_kit_tree

    link_kit_tree(work)
else:
    sys.path.insert(0, "tools/buck2")
    from apple_records import AppleRecords
    from bootstrap_llvm import fetch

    native = Path("tools/native-toolchain")
    lock = json.loads((native / "native.lock.json").read_text())
    pins = lock["link_kit"]
    cache = {}

    def download(pin):
        if pin["sha256"] not in cache:
            url = pin["url"]
            cache[pin["sha256"]] = fetch(dict(pin, name=url.rsplit("/", 1)[1], urls=[url]), work)
        return cache[pin["sha256"]]

    arm = work / "arm"
    with tarfile.open(download(lock["arm"])) as archive:
        archive.extractall(arm, filter="tar")
    (arm_root,) = arm.iterdir()
    kit_pins = work / "pins.json"
    kit_pins.write_text(json.dumps(dict(pins, arm_static=lock["arm"])))
    apple_targets = work / "apple-targets.json"
    apple_targets.write_text(json.dumps(AppleRecords((native / "apple-targets.json").read_text()).rows))
    command = [
        sys.executable, "-B", "tools/buck2/native-toolchain/link_kit.py",
        "--llvm", os.environ["LLVM_SYS_231_PREFIX"], "--arm-sysroot",
        arm_root / "aarch64-none-linux-gnu/libc", "--apple-targets", apple_targets,
        "--pins", kit_pins, "--output", work / "kit",
    ]
    for flag, key in (("abilists", "abilists"), ("libunwind", "libunwind"),
                      ("compiler-rt", "compiler-rt"), ("zig-libsystem", "libSystem-tbd")):
        command.extend((f"--{flag}", download(pins[key])))
    for key, pin in pins.items():
        if ":" in key:
            command.extend(("--deb", f"{key}={download(pin)}"))
    for family in ("darwin", "ios"):
        for library in ("libSystem", "CoreFoundation", "Security"):
            command.extend((f"--{family}-symbols", f"{library}={native / family / (library + '.symbols')}"))
    subprocess.run(command, check=True)
PY

build="$target_dir/$target/release"
"$build/hirsel" core-product --install "$stage"
"$build/hirsel" std-product --install "$stage" --target "$target" \
    --runtime "$target_dir/$target/$runtime_profile/libhirsel_core_rt.a" --link-kit "$work/kit"

# Renaming over an empty directory also handles a prefix made with mktemp -d.
python3 -c 'import os, sys; os.rename(*sys.argv[1:])' "$stage" "$prefix"
stage=
printf 'Installed Hirsel at %s/hirsel\n' "$prefix"
