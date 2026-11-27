#!/bin/sh
set -eu
target=x86_64-unknown-linux-gnu
if [ "${1:-}" = --darwin ]; then
    target=aarch64-apple-darwin
    shift
fi
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$compiler" inspect . --foreign-header hirsel.ai/ffi_laws@1/native "$@" > "$stage/native.h"
if [ "$target" = aarch64-apple-darwin ]; then
    ../../../tools/native-toolchain/darwin/foreign-cc.sh -std=c11 -O2 -fPIC \
        -I "$stage" -c laws.c -o "$stage/laws.o"
    archive=libffi_laws-aarch64-apple-darwin.a
    "$llvm/bin/llvm-ar" --format=darwin rcs "$stage/$archive" "$stage/laws.o"
    cp "$stage/$archive" "$archive"
    exit
fi
"$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 -fPIC -fno-ident \
    -I "$stage" -c laws.c -o "$stage/laws.o"
"$llvm/bin/llvm-ar" rcs "$stage/libffi_laws.a" "$stage/laws.o"
cp "$stage/libffi_laws.a" libffi_laws.a
