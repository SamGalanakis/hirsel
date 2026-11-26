#!/bin/sh
# Sourced by the codec archive recipes. The inputs are full-LTO bitcode;
# capture ld64.lld's native object before it links the temporary dylib.
# LTO drops unreachable code. Localize remaining implementation symbols,
# including C++ operators and allocator wrappers retained by LTO.
darwin_archive() (
    archive=$1 prefix=$2
    shift 2
    "$llvm/bin/ld.lld" -flavor darwin -arch arm64 -platform_version macos 26.0 26.0 \
        -dylib -dead_strip -undefined dynamic_lookup -exported_symbol "_$prefix*" \
        --threads="${HIRSEL_JOBS:-4}" -object_path_lto "$archive.lto" \
        -o "$archive.dylib" "$@"
    object="$archive.lto/0.arm64.lto.o"
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol="_$prefix*" "$object"
    "$llvm/bin/llvm-ar" --format=darwin rcs "$archive" "$object"
)

darwin_compilers() {
    darwin_tools=$(cd ../../.. && pwd)/tools/native-toolchain/darwin
    ln -s "$darwin_tools/foreign-cc.sh" "$stage/darwin-cc"
    ln -s "$darwin_tools/foreign-cc.sh" "$stage/darwin-cxx"
}
