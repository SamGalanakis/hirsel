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
version=3.2.1
source_sha=6f5819c51649bce2571a23b1b0cd41eae2f57fc3f8908c7ec9e7047efb65ca13
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
archive=libhirsel_jpeg.a
if [ -n "$darwin" ]; then
    . ../../../tools/native-toolchain/darwin/foreign-archive.sh
    darwin_compilers
    archive=libhirsel_jpeg-aarch64-apple-darwin.a
fi
"$compiler" inspect . --foreign-header hirsel.ai/image_jpeg@1/native "$@" > "$stage/native.h"
cp hirsel_jpeg.c "$stage"
(
    cd "$stage"
    curl --fail --silent --show-error --location \
        "https://codeload.github.com/libjpeg-turbo/libjpeg-turbo/tar.gz/refs/tags/$version" -o jpeg.tar.gz
    printf '%s  jpeg.tar.gz\n' "$source_sha" | sha256sum --check --status || {
        echo "libjpeg-turbo $version source checksum mismatch" >&2
        exit 1
    }
    tar -xf jpeg.tar.gz
    src="libjpeg-turbo-$version"
    flags="-O2 -fPIC -fno-ident -fno-strict-aliasing -DNO_GETENV -ffile-prefix-map=$PWD=."
    set -- -DCMAKE_C_COMPILER="$llvm/bin/clang" -DCMAKE_C_COMPILER_TARGET=x86_64-unknown-linux-gnu
    require_simd=ON
    if [ -n "$darwin" ]; then
        require_simd=OFF
        set -- -DCMAKE_C_COMPILER="$stage/darwin-cc" -DCMAKE_SYSTEM_NAME=Darwin \
            -DCMAKE_SYSTEM_PROCESSOR=arm64 -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
            -DCMAKE_PROJECT_INCLUDE="$darwin_tools/foreign-lto.cmake" -DWITH_SIMD=OFF
    fi
    cmake -S "$src" -B codec -G Ninja \
        "$@" \
        -DCMAKE_AR="$llvm/bin/llvm-ar" -DCMAKE_RANLIB="$llvm/bin/llvm-ranlib" \
        -DCMAKE_C_FLAGS="$flags" -DCMAKE_C_FLAGS_RELEASE=-DNDEBUG \
        -DBUILD=20261124 -DENABLE_SHARED=OFF -DENABLE_STATIC=ON -DREQUIRE_SIMD="$require_simd" \
        -DWITH_ARITH_DEC=OFF -DWITH_ARITH_ENC=OFF -DWITH_TURBOJPEG=OFF -DWITH_TOOLS=OFF -DWITH_TESTS=OFF
    cmake --build codec --target jpeg-static --parallel "${HIRSEL_JOBS:-4}"
    if [ -n "$darwin" ]; then
        "$stage/darwin-cc" -std=c11 -O2 -flto -fPIC -fno-strict-aliasing -Wall -Wextra -Werror \
            -Wno-unterminated-string-initialization -I codec -I "$src/src" -c hirsel_jpeg.c -o hirsel_jpeg.o
        darwin_archive "$archive" hjpg_ hirsel_jpeg.o codec/libjpeg.a
        exit
    fi
    "$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 \
        -fPIC -fno-ident -fno-strict-aliasing -Wall -Wextra -Werror -Wno-unterminated-string-initialization \
        -I codec -I "$src/src" -c hirsel_jpeg.c -o hirsel_jpeg.o
    "$llvm/bin/ld.lld" -r hirsel_jpeg.o codec/libjpeg.a -o hjpg.o
    # NASM's ELF file symbols contain the temporary source path.
    "$llvm/bin/llvm-objcopy" --strip-debug --wildcard --keep-global-symbol='hjpg_*' hjpg.o
    "$llvm/bin/llvm-objdump" --section-headers hjpg.o > sections
    "$llvm/bin/llvm-readelf" --symbols --relocations --wide hjpg.o > symbols
    "$llvm/bin/llvm-nm" --defined-only --extern-only --format=posix hjpg.o > exports
    if rg -q '\.(init_array|fini_array|preinit_array)(\.|\s)' sections ||
       rg -q 'IRELATIVE|\b(IFUNC|dlopen|dlmopen|dlsym)\b' symbols ||
       rg -qv '^hjpg_' exports; then
        echo 'JPEG archive contains prohibited native features' >&2
        exit 1
    fi
    "$llvm/bin/llvm-ar" rcs libhirsel_jpeg.a hjpg.o
    if [ -n "${JPEG_REFERENCE_DIR:-}" ]; then
        mkdir -p "$JPEG_REFERENCE_DIR"
        cp codec/libjpeg.a codec/jconfig.h "$src/src/jpeglib.h" "$src/src/jmorecfg.h" \
            "$src/src/jerror.h" "$JPEG_REFERENCE_DIR"
        cp -R "$src/testimages" "$JPEG_REFERENCE_DIR"
    fi
)
cp "$stage/$archive" "$archive"
