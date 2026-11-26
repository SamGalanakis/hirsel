#!/bin/sh
set -eu
darwin=
if [ "${1:-}" = --darwin ]; then
    darwin=1
    shift
fi
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
archive=libhirsel_webp.a
if [ -n "$darwin" ]; then
    . ../../../tools/native-toolchain/darwin/foreign-archive.sh
    darwin_compilers
    archive=libhirsel_webp-aarch64-apple-darwin.a
fi
"$compiler" inspect . --foreign-header image_webp@1/native "$@" > "$stage/native.h"
cp hirsel_webp.c "$stage"
(
    cd "$stage"
    curl --fail --silent --show-error --location \
        https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-1.6.0.tar.gz -o libwebp.tar.gz
    echo 'e4ab7009bf0629fd11982d4c2aa83964cf244cffba7347ecd39019a9e38c4564  libwebp.tar.gz' |
        sha256sum --check --status || { echo 'libwebp v1.6.0 source checksum mismatch' >&2; exit 1; }
    tar -xf libwebp.tar.gz
    cd libwebp-1.6.0
    if [ -n "$darwin" ]; then
        cmake -S . -B build -G Ninja -DCMAKE_SYSTEM_NAME=Darwin -DCMAKE_SYSTEM_PROCESSOR=arm64 \
            -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY -DCMAKE_C_COMPILER="$stage/darwin-cc" \
            -DCMAKE_PROJECT_INCLUDE="$darwin_tools/foreign-lto.cmake" \
            -DCMAKE_AR="$llvm/bin/llvm-ar" -DCMAKE_RANLIB="$llvm/bin/llvm-ranlib" \
            -DCMAKE_C_FLAGS="-O2 -std=c11 -fPIC -fno-strict-aliasing -ffile-prefix-map=$PWD=." \
            -DBUILD_SHARED_LIBS=OFF -DWEBP_BUILD_LIBWEBPMUX=OFF -DWEBP_BUILD_ANIM_UTILS=OFF \
            -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF \
            -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_EXTRAS=OFF
        cmake --build build --target webp webpdemux --parallel "${HIRSEL_JOBS:-4}"
        "$stage/darwin-cc" -std=c11 -O2 -flto -fPIC -fno-strict-aliasing -Wall -Wextra -Werror \
            -Wno-unterminated-string-initialization -I src -c ../hirsel_webp.c -o ../hirsel_webp.o
        darwin_archive "../$archive" hwebp_ ../hirsel_webp.o \
            build/libwebpdemux.a build/libwebp.a build/libsharpyuv.a
        exit
    fi
    export CC="$llvm/bin/clang --target=x86_64-unknown-linux-gnu"
    export AR="$llvm/bin/llvm-ar" RANLIB="$llvm/bin/llvm-ranlib"
    export CFLAGS="-O2 -std=c11 -fPIC -fno-ident -fno-strict-aliasing -ffile-prefix-map=$PWD=."
    ./configure --disable-shared --enable-static --disable-libwebpmux --enable-libwebpdemux --enable-threading
    make -j "${HIRSEL_JOBS:-4}" -C sharpyuv
    make -j "${HIRSEL_JOBS:-4}" -C src
    "$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 \
        -fPIC -fno-ident -fno-strict-aliasing -Wall -Wextra -Werror -Wno-unterminated-string-initialization \
        -I src -c ../hirsel_webp.c -o ../hirsel_webp.o
    "$llvm/bin/ld.lld" -r ../hirsel_webp.o src/demux/.libs/libwebpdemux.a \
        src/.libs/libwebp.a sharpyuv/.libs/libsharpyuv.a -o ../hwebp.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hwebp_*' ../hwebp.o
    "$llvm/bin/llvm-ar" rcs ../libhirsel_webp.a ../hwebp.o
    if [ -n "${WEBP_REFERENCE_DIR:-}" ]; then
        mkdir -p "$WEBP_REFERENCE_DIR/include"
        cp -r src/webp "$WEBP_REFERENCE_DIR/include"
        cp src/demux/.libs/libwebpdemux.a src/.libs/libwebp.a sharpyuv/.libs/libsharpyuv.a "$WEBP_REFERENCE_DIR"
    fi
)
cp "$stage/$archive" "$archive"
