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
if [ -n "$darwin" ]; then
    . ../../../tools/native-toolchain/darwin/foreign-archive.sh
    darwin_compilers
fi
"$compiler" inspect . --foreign-header h264@1/native "$@" > "$stage/native.h"
cp hirsel_h264.cpp decoder-allocation.patch "$stage"
(
    cd "$stage"
    if [ -n "${OPENH264_TARBALL:-}" ]; then
        cp "$OPENH264_TARBALL" openh264.tar.gz
    else
        curl --fail --silent --show-error --location \
            https://codeload.github.com/cisco/openh264/tar.gz/refs/tags/v2.7.0 -o openh264.tar.gz
    fi
    echo '5178775c8b440073e70920e97d195efff721c0e56d0a78cc82c919e95d54924d  openh264.tar.gz' |
        sha256sum --check --status || { echo 'OpenH264 v2.7.0 source checksum mismatch' >&2; exit 1; }
    tar -xf openh264.tar.gz
    cd openh264-2.7.0
    patch --batch -p1 < ../decoder-allocation.patch
    cxx="-fPIC -fno-ident -fno-exceptions -fno-rtti -fcheck-new -fno-threadsafe-statics -fno-sized-deallocation"
    printf '%s\n' '#include "native.h"' 'HIRSEL_ABI_TEXT' > ../abi.c
    if [ -n "$darwin" ]; then
        make -j "${HIRSEL_JOBS:-4}" ARCH=arm64 OS=darwin USE_ASM=No \
            CC="$stage/darwin-cc" CXX="$stage/darwin-cxx" AR="$llvm/bin/llvm-ar" \
            CFLAGS_OPT="-O2 -flto -ffile-prefix-map=$PWD=. -DHIRSEL_WRAP_CALLOC -include $darwin_tools/foreign-alloc.h" \
            CXXFLAGS="$cxx" libopenh264.a
        "$stage/darwin-cxx" -std=c++17 -O2 -flto $cxx -Wall -Wextra -Werror \
            -I codec/api/wels -c ../hirsel_h264.cpp -o ../hirsel_h264.o
        "$stage/darwin-cc" -flto -fPIC -c ../abi.c -o ../abi.o
        darwin_archive ../libhirsel_h264-aarch64-apple-darwin.a hh264_ \
            ../hirsel_h264.o ../abi.o libopenh264.a
        exit
    fi
    target=--target=x86_64-unknown-linux-gnu
    make -j "${HIRSEL_JOBS:-4}" ARCH=x86_64 OS=linux \
        CC="$llvm/bin/clang $target" CXX="$llvm/bin/clang++ $target" \
        AR="$llvm/bin/llvm-ar" \
        CFLAGS_OPT="-O2 -fno-ident -ffile-prefix-map=$PWD=." \
        CXXFLAGS="$cxx -DHAVE_AVX2" libopenh264.a
    "$llvm/bin/clang++" "$target" -std=c++17 -O2 $cxx -Wall -Wextra -Werror \
        -I codec/api/wels -c ../hirsel_h264.cpp -o ../hirsel_h264.o
    "$llvm/bin/clang" "$target" -fPIC -fno-ident -c ../abi.c -o ../abi.o
    "$llvm/bin/ld.lld" -r --wrap=malloc --wrap=calloc --wrap=free ../hirsel_h264.o ../abi.o libopenh264.a -o ../hh264.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hh264_*' ../hh264.o
    "$llvm/bin/llvm-objdump" --section-headers ../hh264.o > ../sections
    "$llvm/bin/llvm-readelf" --symbols --relocations ../hh264.o > ../symbols
    if grep -Eq '[[:space:]]\.(init|fini|ctors|dtors|init_array|fini_array|preinit_array)(\.|[[:space:]])' ../sections ||
       grep -Eq '[[:space:]](IFUNC|UNIQUE|R_X86_64_IRELATIVE|dlopen|dlmopen)(@|[[:space:]]|$)' ../symbols; then
        echo 'OpenH264 archive contains an inadmissible ELF feature' >&2
        exit 1
    fi
    "$llvm/bin/llvm-ar" rcs ../libhirsel_h264.a ../hh264.o
    if [ -n "${OPENH264_REFERENCE_DIR:-}" ]; then
        mkdir -p "$OPENH264_REFERENCE_DIR"
        cp libopenh264.a codec/api/wels/*.h "$OPENH264_REFERENCE_DIR"
    fi
)
archive=libhirsel_h264.a
if [ -n "$darwin" ]; then
    archive=libhirsel_h264-aarch64-apple-darwin.a
fi
cp "$stage/$archive" "$archive"
