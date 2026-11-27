#!/bin/bash
# Sourced by the Linux release and reproducibility builds inside manylinux.
dnf -y install bubblewrap dpkg zstd
git config --global --add safe.directory /src
export CARGO_HOME="${CARGO_HOME:-/root/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-/root/.rustup}"
toolchain_bin="$RUSTUP_HOME/toolchains/1.99.0-x86_64-unknown-linux-gnu/bin"
export PATH="$toolchain_bin:$CARGO_HOME/bin:$PATH"
if [[ ! -x "$toolchain_bin/rustc" || ! -x "$toolchain_bin/cargo" ]]; then
  curl --proto "=https" --tlsv1.2 -fsSL https://sh.rustup.rs |
    sh -s -- -y --no-modify-path --profile minimal --default-toolchain 1.99.0
fi
cargo fetch --locked
bolt_tools_dir="${HIRSEL_BOLT_TOOLS_DIR:-/tmp/hirsel-bolt-tools}"
bolt_tools_sha="$(awk '$1 == "bolt_tools" { print $2; exit }' tools/native-toolchain/llvm-23.lock)"
if [[ ! -x "$bolt_tools_dir/bin/llvm-bolt" || ! -f "$bolt_tools_dir/.sha256" || "$(cat "$bolt_tools_dir/.sha256")" != "$bolt_tools_sha" ]]; then
  rm -rf "$bolt_tools_dir"
  tools/native-toolchain/install --bolt-tools "$bolt_tools_dir"
  printf '%s\n' "$bolt_tools_sha" > "$bolt_tools_dir/.sha256"
fi
export PATH="$bolt_tools_dir/bin:$PATH"
