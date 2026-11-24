#!/bin/sh
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$compiler" inspect . --foreign-header opus@1/native "$@" > "$stage/native.h"
cp hirsel_opus.c "$stage"
(
    cd "$stage"
    curl --fail --silent --show-error --location \
        https://downloads.xiph.org/releases/opus/opus-1.5.tar.gz -o opus.tar.gz
    echo 'd8230bbeb99e6d558645aaad25d79de8f4f28fdcc55f8af230050586d62c4f2c  opus.tar.gz' |
        sha256sum --check --status || { echo 'Opus 1.5 source checksum mismatch' >&2; exit 1; }
    tar -xf opus.tar.gz
    cd opus-1.5
    export CC="$llvm/bin/clang --target=x86_64-unknown-linux-gnu"
    export AR="$llvm/bin/llvm-ar" RANLIB="$llvm/bin/llvm-ranlib"
    export CFLAGS="-O2 -std=c11 -fPIC -fno-ident -fno-strict-aliasing -ffile-prefix-map=$PWD=."
    ./configure --disable-shared --enable-static --disable-doc
    make -j "${HIRSEL_JOBS:-4}" libopus.la
    "$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 \
        -fPIC -fno-ident -fno-strict-aliasing -Wall -Wextra -Werror -Wno-unterminated-string-initialization \
        -I include -c ../hirsel_opus.c -o ../hirsel_opus.o
    "$llvm/bin/ld.lld" -r ../hirsel_opus.o .libs/libopus.a -o ../hop.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hop_*' ../hop.o
    "$llvm/bin/llvm-ar" rcs ../libhirsel_opus.a ../hop.o
    if [ -n "${OPUS_REFERENCE_DIR:-}" ]; then
        make -j "${HIRSEL_JOBS:-4}" opus_demo opus_compare
        mkdir -p "$OPUS_REFERENCE_DIR"
        cp .libs/libopus.a opus_demo opus_compare include/opus*.h "$OPUS_REFERENCE_DIR"
    fi
)
cp "$stage/libhirsel_opus.a" libhirsel_opus.a
