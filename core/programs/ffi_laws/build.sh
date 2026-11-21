#!/bin/sh
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$compiler" inspect . --foreign-header ffi_laws@1/native "$@" > "$stage/native.h"
"$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 -fPIC -fno-ident \
    -I "$stage" -c laws.c -o "$stage/laws.o"
"$llvm/bin/llvm-ar" rcs "$stage/libffi_laws.a" "$stage/laws.o"
cp "$stage/libffi_laws.a" libffi_laws.a
