#!/bin/bash
set -euo pipefail
os=$(uname -s)
arch=$(uname -m)
if [[ "$os" != Darwin || "$arch" != arm64 ]]; then
  echo "Expected macOS 26 on arm64, got $os/$arch" >&2
  exit 1
fi
version=$(sw_vers -productVersion)
if [[ "${version%%.*}" != 26 ]]; then
  echo "Expected macOS 26, got $version" >&2
  exit 1
fi
echo "macOS $version on $arch"
