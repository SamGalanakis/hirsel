#!/usr/bin/env bash
# Populate ignored, checksum-pinned dependencies once. Subsequent builds are offline.
set -euo pipefail
cd -- "$(dirname -- "$0")"
cargo vendor --locked vendor >/dev/null
printf '%s\n' 'Dependencies ready. Build with: cargo build --offline --locked --release'
