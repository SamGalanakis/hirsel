#!/bin/sh
# Sourced by the codec archive recipes. The inputs are full-LTO bitcode;
# capture ld64.lld's native object before it links the temporary dylib.
# LTO drops unreachable code. Localize remaining implementation symbols,
# including C++ operators and allocator wrappers retained by LTO.
darwin_archive() (
    archive=$1 prefix=$2
    shift 2
    "$llvm/bin/ld.lld" -flavor darwin -arch arm64 \
        -platform_version "$darwin_platform" "$darwin_floor" "$darwin_floor" \
        -dylib -dead_strip -undefined dynamic_lookup -exported_symbol "_$prefix*" \
        --threads="${HIRSEL_JOBS:-4}" -object_path_lto "$archive.lto" \
        -o "$archive.dylib" "$@"
    object="$archive.lto/0.arm64.lto.o"
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol="_$prefix*" "$object"
    "$llvm/bin/llvm-ar" --format=darwin rcs "$archive" "$object"
)

darwin_compilers() {
    repo=$(cd ../../.. && pwd)
    darwin_tools=$repo/tools/native-toolchain/darwin
    darwin_record=$(python3 "$repo/tools/buck2/apple_records.py" macos llvm_triple platform_version_name floor)
    read -r HIRSEL_APPLE_LLVM_TRIPLE darwin_platform darwin_floor <<EOF
$darwin_record
EOF
    export HIRSEL_APPLE_LLVM_TRIPLE
    ln -s "$darwin_tools/foreign-cc.sh" "$stage/darwin-cc"
    ln -s "$darwin_tools/foreign-cc.sh" "$stage/darwin-cxx"
}
