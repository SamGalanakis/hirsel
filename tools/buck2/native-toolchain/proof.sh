#!/bin/bash
# Run the tools with positive output checks and a deliberately leaking twin.
set -euo pipefail
llvm=$(realpath "$1")
arm=$(realpath "$2")
qemu=$(realpath "$3")
valgrind=$(realpath "$4")
kit=$(realpath "$5")
darwin=$6
export LD_LIBRARY_PATH="$llvm/lib"
export VALGRIND_LIB="$valgrind/libexec/valgrind"
build=$(mktemp -d)
trap 'rm -rf "$build"' EXIT
fail() { echo "native toolchain proof: $*" >&2; exit 1; }
version=$("$llvm/bin/llvm-config" --version)
clang_version=$("$llvm/bin/clang" --version)
[[ "${clang_version%%$'\n'*} " == *"clang version $version "* ]] || fail "clang version differs from the pinned LLVM"
printf '%s\n' "$clang_version"
lld_version=$("$llvm/bin/ld.lld" --version)
[[ "$lld_version" == *"LLD $version "* ]] || fail "lld version differs from the pinned LLVM"
printf '%s\n' "$lld_version"
cat > "$build/tiny.c" <<'C'
#include <stdio.h>
#include <stdlib.h>
static void *volatile allocation;
int main(void) {
  allocation = malloc(32);
  if (!allocation) return 2;
  *(volatile unsigned char *)allocation = 7;
#ifndef PROOF_LEAK
  free(allocation);
#endif
  allocation = NULL;
  puts("native-toolchain-smoke");
  return 0;
}
C
link_native() {
  "$llvm/bin/clang" --ld-path="$llvm/bin/ld.lld" -O0 -g "$build/tiny.c" \
    -Xlinker --dynamic-linker -Xlinker "$valgrind/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2" \
    -Xlinker -rpath -Xlinker "$valgrind/lib/x86_64-linux-gnu" "$@"
}
link_native -o "$build/tiny"
[[ "$("$build/tiny")" == native-toolchain-smoke ]] || fail "the lld-linked executable did not print its marker"
echo 'x86-64: linked with input lld and printed native-toolchain-smoke'
"$arm/bin/aarch64-none-linux-gnu-gcc" -static -O0 -g "$build/tiny.c" -o "$build/arm"
[[ "$("$qemu/qemu-aarch64" "$build/arm")" == native-toolchain-smoke ]] || fail "the AArch64 qemu run did not print its marker"
echo 'AArch64: cross-compiled executable printed native-toolchain-smoke under input qemu'
memcheck=("$valgrind/bin/valgrind" --error-exitcode=97 --leak-check=full --errors-for-leak-kinds=definite)
"${memcheck[@]}" "$build/tiny" > "$build/good.out" 2> "$build/good.err"
[[ "$(cat "$build/good.out")" == native-toolchain-smoke ]] || fail "Valgrind did not run the accepted executable"
grep -Fq 'ERROR SUMMARY: 0 errors from 0 contexts' "$build/good.err" || { cat "$build/good.err" >&2; fail "Valgrind did not report zero errors"; }
grep -Fq 'in use at exit: 0 bytes in 0 blocks' "$build/good.err" || fail "the accepted executable leaked"
cat "$build/good.err"
link_native -DPROOF_LEAK -o "$build/leak"
status=0
"${memcheck[@]}" "$build/leak" > "$build/leak.out" 2> "$build/leak.err" || status=$?
[[ "$status" == 97 ]] || { cat "$build/leak.err" >&2; fail "the planted leak exited $status instead of 97"; }
[[ "$(cat "$build/leak.out")" == native-toolchain-smoke ]] || fail "Valgrind did not run the leaking twin"
grep -Fq 'definitely lost: 32 bytes in 1 blocks' "$build/leak.err" || { cat "$build/leak.err" >&2; fail "Valgrind did not find the planted 32-byte leak"; }
grep -Eq 'ERROR SUMMARY: [1-9][0-9]* errors' "$build/leak.err" || fail "Valgrind did not count the planted leak as an error"
cat "$build/leak.err"
# Mach-O: link for aarch64-apple-darwin against every library stub the kit generates
# with the same ld.lld (§4.2 of the macOS design; nothing runs, there is no Mac here).
# A framework the program never reaches leaves no load command.
cat > "$build/darwin.c" <<'C'
void *malloc(unsigned long); void free(void *); long write(int, const void *, unsigned long); int fsync(int);
int SecTrustSettingsCopyCertificates(int, void **);
int main(void) { void *p = malloc(32); free(p); write(1, "darwin\n", 7);
#ifdef PROOF_FSYNC
  fsync(1);
#endif
#ifdef PROOF_KEYCHAIN
  SecTrustSettingsCopyCertificates(0, &p);
#endif
  return 0; }
C
darwin_flags=$(python3 -c 'import json,sys
row = json.load(open(sys.argv[1]))["targets"][sys.argv[2]]
print(row["floor"], row["llvm_triple"], row["platform_version_name"])
' "$kit/manifest.json" "$darwin")
read -r floor llvm_triple darwin_platform <<< "$darwin_flags"
mapfile -t darwin_libraries < <(python3 -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["targets"][sys.argv[2]]["libraries"]))' "$kit/manifest.json" "$darwin")
link_darwin() {
  "$llvm/bin/clang" --target="$llvm_triple" -c "$build/darwin.c" -o "$build/darwin.o" "$@"
  "$kit/ld.lld" -flavor darwin -arch arm64 -platform_version "$darwin_platform" "$floor" "$floor" -dead_strip -dead_strip_dylibs \
    -adhoc_codesign "$build/darwin.o" "${darwin_libraries[@]/#/$kit/$darwin/}" -o "$build/darwin"
}
link_darwin
headers=$("$llvm/bin/llvm-objdump" --macho --private-headers "$build/darwin")
[[ "$(grep -E '^ +minos ' <<< "$headers" | awk '{print $2}')" == "$floor" ]] || fail "LC_BUILD_VERSION minos is not the floor $floor"
[[ "$(grep -E '^ +name /' <<< "$headers" | awk '{print $2}' | sort | tr '\n' ' ')" == "/usr/lib/dyld /usr/lib/libSystem.B.dylib " ]] || fail "the Mach-O loads something besides libSystem"
grep -q 'cmd LC_CODE_SIGNATURE' <<< "$headers" || fail "the Mach-O is not ad-hoc signed"
[[ "$("$llvm/bin/llvm-nm" -u "$build/darwin" | awk '{print $NF}' | tr '\n' ' ')" == "_free _malloc _write " ]] || fail "the Mach-O imports differ from free, malloc and write"
echo "aarch64-apple-darwin: linked against libSystem.tbd, minos $floor, ad-hoc signed, imports free malloc write"
status=0
link_darwin -DPROOF_FSYNC 2> "$build/fsync.err" || status=$?
[[ "$status" != 0 ]] && grep -q 'undefined symbol: _fsync' "$build/fsync.err" || { cat "$build/fsync.err" >&2; fail "a Darwin program importing fsync linked"; }
echo 'aarch64-apple-darwin: a program importing fsync is refused at link time'
link_darwin -DPROOF_KEYCHAIN
loads=$("$llvm/bin/llvm-objdump" --macho --private-headers "$build/darwin" | grep -E '^ +name /' | awk '{print $2}' | sort | tr '\n' ' ')
[[ "$loads" == "/System/Library/Frameworks/Security.framework/Versions/A/Security /usr/lib/dyld /usr/lib/libSystem.B.dylib " ]] || fail "a Keychain read loads $loads, not Security and libSystem"
echo 'aarch64-apple-darwin: a Keychain read loads Security.framework; a program without one does not'
echo 'native toolchain proof: clang, lld (ELF and Mach-O), cross gcc, qemu and Valgrind passed; planted leak refused with exit 97'
