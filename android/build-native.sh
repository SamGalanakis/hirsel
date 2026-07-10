#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source /workspace/android/env.sh
cd "$repo_root"

output="$repo_root/android/app/src/main/jniLibs"
bindings="$repo_root/android/app/src/main/kotlin"
rm -rf "$output/arm64-v8a" "$output/x86_64" "$bindings/uniffi"

cargo ndk \
    --target arm64-v8a \
    --target x86_64 \
    --platform 26 \
    --output-dir "$output" \
    build --release -p hirsel-client-ffi

cargo build --release -p hirsel-client-ffi
target_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)"
cargo run --release -p hirsel-client-ffi --features bindgen-cli --bin uniffi-bindgen -- \
    generate \
    --library "$target_dir/release/libhirsel_client_ffi.so" \
    --language kotlin \
    --config "$repo_root/crates/hirsel-client-ffi/uniffi.toml" \
    --out-dir "$bindings" \
    --no-format
