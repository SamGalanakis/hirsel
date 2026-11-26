#!/bin/bash
# Sourced by the Linux release and reproducibility builds inside manylinux.
dnf -y install bubblewrap dpkg zstd
git config --global --add safe.directory /src
curl --proto "=https" --tlsv1.2 -fsSL https://sh.rustup.rs |
  sh -s -- -y --profile minimal --default-toolchain 1.99.0
export PATH="/root/.cargo/bin:$PATH"
cargo fetch --locked
tools/native-toolchain/install --bolt-tools /tmp/hirsel-bolt-tools
export PATH="/tmp/hirsel-bolt-tools/bin:$PATH"
