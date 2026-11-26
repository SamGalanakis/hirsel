#!/bin/sh
set -eu
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
headers=${ZIG_DARWIN_INCLUDE:?set ZIG_DARWIN_INCLUDE to the pinned Zig Darwin headers}
compiler=clang
if [ "${0##*/}" = darwin-cxx ]; then
    compiler=clang++
    cxx=${ZIG_LIBCXX_INCLUDE:-"$headers/../../../libcxx/include"}
    set -- -nostdinc++ -isystem "$cxx" \
        -D_LIBCPP_HARDENING_MODE_DEFAULT=_LIBCPP_HARDENING_MODE_NONE \
        -D_LIBCPP_ASSERTION_SEMANTIC_DEFAULT=_LIBCPP_ASSERTION_SEMANTIC_IGNORE "$@"
fi
exec "$llvm/bin/$compiler" --target=arm64-apple-macos26.0 -nostdlibinc \
    -D_FORTIFY_SOURCE=0 -fno-ident -fno-autolink "$@" -isystem "$headers"
