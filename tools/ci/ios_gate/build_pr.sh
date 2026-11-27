#!/bin/bash
# Build the pinned comparison compiler and its SwiftUI host inside the disposable checkout.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
checkout="$1"
output="$2"
host="$checkout/host-source"
workspace="$checkout/comparison"
app="$workspace/app"
llvm="$checkout/tools/buck2/toolchains/llvm-files"
export LLVM_SYS_231_PREFIX="$llvm"
export MACOSX_DEPLOYMENT_TARGET=26.0
export IPHONEOS_DEPLOYMENT_TARGET=18.0

cd "$checkout"
git init -q
git remote add origin https://github.com/SamGalanakis/hirsel.git
pr_commit=65a6f54ae9e7c11eea8934cbfa11c502aa856a49
git fetch --depth=1 origin pull/11/head
if [[ "$(git rev-parse FETCH_HEAD)" != "$pr_commit" ]]; then
  git fetch --depth=1 origin "$pr_commit"
fi
git checkout --detach "$pr_commit"
git init -q "$host"
git -C "$host" remote add origin https://github.com/gillesmag/hirsel-ios.git
git -C "$host" fetch --depth=1 origin 30f36dae1a78c76a06fac86e97bb9276ac8b827f
git -C "$host" checkout --detach FETCH_HEAD

rustup toolchain install --no-self-update
rustup target add aarch64-apple-ios-sim
python3 tools/buck2/bootstrap_llvm.py
cargo build --locked --release -p hirsel-core-driver --bin hirsel
cargo rustc --locked --release -p hirsel-core-rt --lib --features standalone --crate-type staticlib
cargo rustc --locked --release -p hirsel-core-rt --lib --features standalone --crate-type staticlib --target aarch64-apple-ios-sim

# The host repo's installation script expects these paths in its compiler checkout.
mkdir -p target/macos-release/runtimes/default target/ios-runtime/aarch64-apple-ios-sim/release
ln -s "$llvm" target/macos-release/llvm-23.1.5
ln -s "$checkout/target/release/libhirsel_core_rt.a" target/macos-release/runtimes/default/libhirsel_core_rt.a
ln -s "$checkout/target/aarch64-apple-ios-sim/release/libhirsel_core_rt.a" \
  target/ios-runtime/aarch64-apple-ios-sim/release/libhirsel_core_rt.a
HIRSEL_REPO="$checkout" python3 "$host/cli/setup-toolchain.py"

mkdir -p "$app"
cp -R "$host/package" "$workspace/ios"
cp -R "$checkout/core/programs/store" "$workspace/store"
cat > "$workspace/hirsel.toml" <<'TOML'
[workspace]
members = ["app", "ios", "store"]
TOML
cp "$here/pr_counter.hirsel" "$app/main.hirsel"
cp "$here/counter.hirsel" "$app/counter.hirsel"
cat > "$app/hirsel.toml" <<'TOML'
[package]
name = "pr_counter"
major = 1
charge = "charge-v1"

[dependencies]
std = { major = 1 }
ios = { major = 1, path = "../ios" }
store = { major = 1, path = "../store" }

[sources]
"main.hirsel" = "main"
"counter.hirsel" = "counter"
TOML
"$host/.toolchain/hirsel" build "$app" --libraries "$checkout/core" --target aarch64-apple-ios-sim \
  --staticlib --limit 1073741824 --grant Tasks --grant Clock --grant Random \
  --grant Foreign=ios@1/ios/Ios --dir xdg-data:counter -o "$app/libpr_counter.a"

mkdir "$output"
cat > "$app/bridge.h" <<HEADER
#include "$here/Memory.h"
#include "$host/bridge/hirsel_ios_bridge.h"
HEADER
sdk="$(xcrun --sdk iphonesimulator --show-sdk-path)"
xcrun --sdk iphonesimulator swiftc -O -D PR_COUNTER -target arm64-apple-ios26.0-simulator \
  -sdk "$sdk" -import-objc-header "$app/bridge.h" \
  "$here/CounterHost.swift" "$host/host/HirselProtocol.swift" "$app/libpr_counter.a" \
  -framework Security -framework CoreFoundation -o "$output/PRCounter"
python3 - "$output" <<'PY'
from pathlib import Path
import plistlib
import sys

with (Path(sys.argv[1]) / "Info.plist").open("wb") as stream:
    plistlib.dump({
        "CFBundleIdentifier": "org.hirsel.simulator-counter-pr11",
        "CFBundleName": "PRCounter",
        "CFBundleExecutable": "PRCounter",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "1.0",
        "CFBundleVersion": "1",
        "MinimumOSVersion": "26.0",
        "LSRequiresIPhoneOS": True,
        "UIDeviceFamily": [1],
    }, stream)
PY
codesign --sign - "$output"
