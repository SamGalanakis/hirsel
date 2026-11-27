#!/bin/sh
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
target=${YUV_TARGET:-x86_64-unknown-linux-gnu}
if [ "${1:-}" = --darwin ]; then
    target=aarch64-apple-darwin
    shift
fi
system=Linux
cc="$llvm/bin/clang"
cxx="$llvm/bin/clang++"
case "$target" in
    x86_64-unknown-linux-gnu) arch=x86_64; arch_flags= ;;
    aarch64-unknown-linux-gnu)
        arch=aarch64
        arch_flags="--sysroot=${YUV_AARCH64_SYSROOT:-/usr/aarch64-linux-gnu}"
        ;;
    aarch64-apple-darwin) system=Darwin; arch=arm64; arch_flags= ;;
    *) echo "Unsupported libyuv target: $target" >&2; exit 1 ;;
esac

# Upstream LIBYUV_VERSION 1985. The canonical repository has no release tags.
revision=525a3a3c726caf41c1e407c040b698cbb45e188c
source_sha=0df0780ea3c7695385a7e003b4e08f2b4150d1e52781b723602a8adf85f83c29
stage=$(mktemp -d)
checkout=$(mktemp -d /tmp/ref-libyuv.XXXXXX)
trap 'rm -rf "$stage" "$checkout"' EXIT
archive="libhirsel_yuv-$arch.a"
if [ "$system" = Darwin ]; then
    . ../../../tools/native-toolchain/darwin/foreign-archive.sh
    darwin_compilers
    cc="$stage/darwin-cc"
    cxx="$stage/darwin-cxx"
    archive=libhirsel_yuv-aarch64-apple-darwin.a
fi
"$compiler" inspect . --foreign-header hirsel.ai/video_yuv@1/native "$@" > "$stage/native.h"
cp hirsel_yuv.c "$stage"
if [ -n "${YUV_REFERENCE_DIR:-}" ]; then
    mkdir -p "$YUV_REFERENCE_DIR"
    reference=$(cd "$YUV_REFERENCE_DIR" && pwd)
fi
(
    cd "$stage"
    git -C "$checkout" init --quiet
    git -C "$checkout" fetch --quiet --depth=1 \
        https://chromium.googlesource.com/libyuv/libyuv "$revision"
    git -C "$checkout" -c tar.umask=0002 archive --format=tar "$revision" > libyuv.tar
    echo "$source_sha  libyuv.tar" | sha256sum --check --status ||
        { echo 'libyuv source checksum mismatch' >&2; exit 1; }
    mkdir source
    tar -xf libyuv.tar -C source
    flags="-O3 -DNDEBUG -ffunction-sections -fdata-sections -fPIC -fno-ident -fno-strict-aliasing -ffile-prefix-map=$PWD=."
    if [ "$system" = Darwin ]; then
        set -- -DCMAKE_PROJECT_INCLUDE="$darwin_tools/foreign-lto.cmake" \
            -DCMAKE_CXX_FLAGS="$flags -fno-exceptions -fno-rtti -include $darwin_tools/foreign-alloc.h"
    else
        set -- -DCMAKE_C_COMPILER_TARGET="$target" -DCMAKE_CXX_COMPILER_TARGET="$target" \
            -DCMAKE_CXX_FLAGS="$flags $arch_flags -fno-exceptions -fno-rtti"
    fi
    cmake -S source -B build -G Ninja -DCMAKE_SYSTEM_NAME="$system" \
        -DCMAKE_SYSTEM_PROCESSOR="$arch" -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
        -DCMAKE_C_COMPILER="$cc" -DCMAKE_CXX_COMPILER="$cxx" \
        -DCMAKE_AR="$llvm/bin/llvm-ar" -DCMAKE_RANLIB="$llvm/bin/llvm-ranlib" \
        -DCMAKE_C_FLAGS="$flags $arch_flags" "$@" \
        -DLIBYUV_DISABLE_JPEG=ON -DCAN_COMPILE_SME=OFF
    cmake --build build --target yuv --parallel "${HIRSEL_JOBS:-4}"
    if [ "$system" = Darwin ]; then
        "$cc" -std=c11 -O3 -flto -fPIC -fno-strict-aliasing -Wall -Wextra -Werror \
            -Wno-unterminated-string-initialization -I source/include -c hirsel_yuv.c
        darwin_archive "$archive" hyuv_ hirsel_yuv.o build/libyuv.a
        exit
    fi
    # shellcheck disable=SC2086
    "$llvm/bin/clang" --target="$target" $arch_flags -std=c11 -O3 -fPIC \
        -fno-ident -fno-strict-aliasing -Wall -Wextra -Werror \
        -Wno-unterminated-string-initialization -I source/include -c hirsel_yuv.c
    cat > sections.ld <<'LINK'
SECTIONS { .hirsel_abi : { KEEP(*(.hirsel_abi)) } }
INSERT AFTER .text;
LINK
    "$llvm/bin/ld.lld" -r --gc-sections --wrap=malloc --wrap=free \
        -u hyuv_to_rgb -u hyuv_from_rgb -u hyuv_scale -u hyuv_rotate \
        -T sections.ld hirsel_yuv.o build/libyuv.a -o hyuv.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hyuv_*' hyuv.o
    "$llvm/bin/llvm-ar" rcs libhirsel_yuv.a hyuv.o
    "$llvm/bin/llvm-objdump" -h libhirsel_yuv.a > sections
    "$llvm/bin/llvm-readelf" --wide --symbols --relocations libhirsel_yuv.a > symbols
    if grep -Eq '\.(init|fini|preinit_array|ctors|dtors)([_.[:space:]]|$)' sections ||
       grep -Eq 'IFUNC|IRELATIVE|(^|[[:space:]])(dlopen|dlsym)([[:space:]]|$)' symbols; then
        echo 'libyuv archive has a constructor, indirect function or dynamic loader reference' >&2
        exit 1
    fi
    "$llvm/bin/llvm-nm" --defined-only --extern-only --just-symbol-name libhirsel_yuv.a > exports
    awk 'NF == 1 && $1 !~ /:$/ { print $1 }' exports | LC_ALL=C sort > sorted-exports
    printf '%s\n' hyuv_from_rgb hyuv_rotate hyuv_scale hyuv_to_rgb > expected-exports
    cmp sorted-exports expected-exports
    if [ -n "${reference:-}" ]; then
        cp build/libyuv.a "$reference"
        cp -R source/include "$reference"
    fi
)
if [ "$system" = Darwin ]; then
    cp "$stage/$archive" "$archive"
else
    cp "$stage/libhirsel_yuv.a" "$archive"
fi
