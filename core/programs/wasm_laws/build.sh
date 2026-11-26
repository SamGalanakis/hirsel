#!/bin/sh
# Rebuilds box.wasm from box.c and box_traps.s byte for byte (FFI-10), then
# repins it in hirsel.toml. CLANG, WASM_LD and LLVM_OBJCOPY name LLVM 21 tools
# with the WebAssembly target.
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
clang=${CLANG:?set CLANG to an LLVM 21 clang with the wasm32 target}
wasm_ld=${WASM_LD:?set WASM_LD to LLVM 21 wasm-ld}
objcopy=${LLVM_OBJCOPY:?set LLVM_OBJCOPY to LLVM 21 llvm-objcopy}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$compiler" inspect . --foreign-header wasm_laws@1/native "$@" > "$stage/native.h"
"$compiler" inspect . --foreign-abi wasm_laws@1/native "$@" > "$stage/abi.txt"
"$clang" --target=wasm32-unknown-unknown -nostdlib -O2 -fno-ident -fno-builtin \
    -ffile-prefix-map="$PWD"=. -I "$stage" -c box.c -o "$stage/box.o"
"$clang" --target=wasm32-unknown-unknown -c box_traps.s -o "$stage/box_traps.o"
exports=$(sed -n 's/^pub fn \([a-z_0-9]*\)(.*/--export=hwl_\1/p' native.hirsel)
# --table-base=3 makes the table 4 elements: slots 0 to 2 null, slot 3 slot_target.
"$wasm_ld" --no-entry --strip-debug --stack-first -z stack-size=16384 \
    --initial-memory=131072 --table-base=3 $exports --export=hwl_loan_area \
    "$stage/box.o" "$stage/box_traps.o" -o "$stage/box.wasm"
"$objcopy" --add-section=hirsel_abi="$stage/abi.txt" "$stage/box.wasm"
cp "$stage/box.wasm" box.wasm
pin=$(sha256sum box.wasm | cut -d' ' -f1)
sed -i "s/^module = .*/module = { file = \"box.wasm\", sha256 = \"$pin\" }/" hirsel.toml
