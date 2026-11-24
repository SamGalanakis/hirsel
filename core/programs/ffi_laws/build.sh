#!/bin/sh
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# HIRSEL_TARGET=aarch64-apple-darwin builds the macOS archive beside the Linux one.
target=${HIRSEL_TARGET:-x86_64-unknown-linux-gnu}
case "$target" in
    aarch64-apple-darwin)
        flags="--target=arm64-apple-macos11 -isysroot $(xcrun --sdk macosx --show-sdk-path)"
        format=darwin
        out=libffi_laws.$target.a
        ;;
    *)
        flags="--target=$target -fPIC"
        format=gnu
        out=libffi_laws.a
        ;;
esac
"$compiler" inspect . --foreign-header ffi_laws@1/native "$@" > "$stage/native.h"
# shellcheck disable=SC2086
"$llvm/bin/clang" $flags -std=c11 -O2 -fno-ident \
    -I "$stage" -c laws.c -o "$stage/laws.o"
"$llvm/bin/llvm-ar" --format=$format rcs "$stage/libffi_laws.a" "$stage/laws.o"
cp "$stage/libffi_laws.a" "$out"
