#!/bin/bash
# Rebuilds libhirsel_dav1d.a and libhirsel_svtav1.a from pinned upstream tags.
# Tools: meson 1.9.1 (MESON, default `uv tool run --from meson==1.9.1 meson`),
# NASM 2.16.01, CMake 3.28.3, Ninja 1.11.1 and the pinned LLVM tree.
set -euo pipefail
codec=$(cd "$(dirname "$0")" && pwd)
cd "$codec"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
if [ -n "${MESON:-}" ]; then meson=("$MESON"); else meson=(uv tool run --from meson==1.9.1 meson); fi
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

require() {
    local have=$1 want=$2
    [ "$have" = "$want" ] || { echo "need $want, found $have" >&2; exit 1; }
}
require "$("${meson[@]}" --version)" 1.9.1
require "$(nasm -v | cut -d' ' -f3)" 2.16.01
require "$(cmake --version | head -1 | cut -d' ' -f3)" 3.28.3
require "$(ninja --version)" 1.11.1

# Each source is the named tag at the named commit.
source_tree() {
    local name=$1 url=$2 tag=$3 commit=$4 checkout=$5
    if [ -z "$checkout" ]; then
        checkout="$stage/$name.git"
        git clone --quiet --bare --depth 1 --branch "$tag" "$url" "$checkout"
    fi
    require "$(git -C "$checkout" rev-parse "$tag^{commit}")" "$commit"
    git -C "$checkout" archive --format=tar --prefix="$name/" "$commit" | tar -xf - -C "$stage"
}

source_tree dav1d https://code.videolan.org/videolan/dav1d.git 1.5.5 \
    6c7bee685677fd997a431302be4fea8ea4017606 "${DAV1D_SOURCE:-}"
source_tree svt-av1 https://gitlab.com/AOMediaCodec/SVT-AV1.git v4.3.0 \
    e3d63405f9ba8219913a7276c2c47d10e5d022fe "${SVT_AV1_SOURCE:-}"
patch --quiet -d "$stage/svt-av1" -p1 < svt-once.patch

# Header generation reads the manifest, whose archive paths must exist.
mkdir "$stage/package"
cp hirsel.toml ./*.hirsel "$stage/package"
"$llvm/bin/llvm-ar" cr "$stage/package/libhirsel_dav1d.a"
"$llvm/bin/llvm-ar" cr "$stage/package/libhirsel_svtav1.a"
"$compiler" inspect "$stage/package" --foreign-header av1@1/dav "$@" > "$stage/dav.h"
"$compiler" inspect "$stage/package" --foreign-header av1@1/svt "$@" > "$stage/svt.h"

export CC="$llvm/bin/clang" CXX="$llvm/bin/clang++"
export AR="$llvm/bin/llvm-ar" RANLIB="$llvm/bin/llvm-ranlib"
export CFLAGS="--target=x86_64-unknown-linux-gnu -fno-ident -ffile-prefix-map=$stage=."
"${meson[@]}" setup "$stage/dav1d-build" "$stage/dav1d" --buildtype=release \
    --default-library=static -Denable_tools=false -Denable_tests=false -Dlogging=false
ninja -C "$stage/dav1d-build" -j "${HIRSEL_JOBS:-4}" src/libdav1d.a
cmake -S "$stage/svt-av1" -B "$stage/svt-build" -G Ninja \
    -DCMAKE_C_FLAGS="$CFLAGS -Qunused-arguments" -DCMAKE_AR="$AR" -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DBUILD_APPS=OFF -DEXCLUDE_HASH=ON \
    -DSVT_AV1_LTO=OFF -DENABLE_FAST_DECODE_3=ON -DLOG_QUIET=ON -DHAVE_VALGRIND_H=OFF
cmake --build "$stage/svt-build" --target SvtAv1Enc -j "${HIRSEL_JOBS:-4}"

pack() {
    local name=$1 prefix=$2 upstream=$3 include=$4
    "$CC" --target=x86_64-unknown-linux-gnu -std=c11 -O2 -fPIC -fno-ident \
        -Wall -Wextra -Werror -Wno-unterminated-string-initialization -I "$stage" -isystem "$include" \
        -c "hirsel_$name.c" -o "$stage/$name-adapter.o"
    "$llvm/bin/ld.lld" -r "$stage/$name-adapter.o" "$upstream" -o "$stage/$name.o"
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol="${prefix}*" \
        --strip-unneeded "$stage/$name.o"
    "$AR" rcsD "$stage/libhirsel_$name.a" "$stage/$name.o"
}

svt=$(find "$stage/svt-av1/Bin" -type f -name libSvtAv1Enc.a -print -quit)
pack dav1d hdav_ "$stage/dav1d-build/src/libdav1d.a" "$stage/dav1d/include"
pack svtav1 hsvt_ "$svt" "$stage/svt-av1/Source/API"
./check-archives.sh "$stage"
cp "$stage/libhirsel_dav1d.a" "$stage/libhirsel_svtav1.a" .

if [ -n "${AV1_REFERENCE_DIR:-}" ]; then
    mkdir -p "$AV1_REFERENCE_DIR"
    cp "$stage/dav1d-build/src/libdav1d.a" "$svt" "$AV1_REFERENCE_DIR"
    cp -r "$stage/dav1d/include/dav1d" "$AV1_REFERENCE_DIR"
    cp "$stage/svt-av1"/Source/API/*.h "$AV1_REFERENCE_DIR"
fi
