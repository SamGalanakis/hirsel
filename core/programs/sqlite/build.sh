#!/bin/sh
set -eu
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$compiler" inspect . --foreign-header sqlite@1/native "$@" > "$stage/native.h"
cp hirsel_sqlite.c "$stage"
(
    cd "$stage"
    curl --fail --silent --show-error --location \
        https://sqlite.org/2026/sqlite-amalgamation-3540100.zip -o amalgamation.zip
    expected=92281c86e2957f9b9305890f9ac04204caa7b677cc868dc326d942689df28e30
    observed=$(openssl dgst -sha3-256 -r amalgamation.zip)
    if [ "${observed%% *}" != "$expected" ]; then
        echo "SQLite 3.54.1 SHA3-256: expected $expected, found ${observed%% *}" >&2
        exit 1
    fi
    unzip -q -j amalgamation.zip '*/sqlite3.c' '*/sqlite3.h'
    for file in sqlite3 hirsel_sqlite; do
        "$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 \
            -fPIC -fno-ident "-ffile-prefix-map=$PWD=." \
            -DSQLITE_OS_OTHER=1 -DSQLITE_THREADSAFE=1 -DSQLITE_TEMP_STORE=3 \
            -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_DESERIALIZE -DSQLITE_OMIT_LOCALTIME \
            -DSQLITE_USE_URI=0 -DSQLITE_DQS=0 -DSQLITE_DEFAULT_FOREIGN_KEYS=1 \
            -c "$file.c" -o "$file.o"
    done
    "$llvm/bin/ld.lld" -r sqlite3.o hirsel_sqlite.o -o hsq.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hsq_*' hsq.o
    "$llvm/bin/llvm-ar" rcs libhirsel_sqlite.a hsq.o
)
cp "$stage/libhirsel_sqlite.a" libhirsel_sqlite.a
