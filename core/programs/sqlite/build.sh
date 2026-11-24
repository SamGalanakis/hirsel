#!/bin/sh
set -eu
test_root=
if [ "${1:-}" = --test ]; then
    test_root=$(cd "$2" && pwd)
    shift 2
fi
cd "$(dirname "$0")"
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
stage=$(mktemp -d)
test_dir=
trap 'rm -rf "$stage" ${test_dir:+"$test_dir"}' EXIT
"$compiler" inspect . --foreign-header sqlite@1/native "$@" > "$stage/native.h"
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
)
if [ -n "$test_root" ]; then
    # SQLite's public C API stays local in the shipped archive.
    "$llvm/bin/llvm-objcopy" \
        --globalize-symbol=sqlite3_auto_extension --globalize-symbol=sqlite3_db_config \
        --globalize-symbol=sqlite3_db_status --globalize-symbol=sqlite3_file_control \
        --globalize-symbol=sqlite3_vfs_find libhirsel_sqlite.a "$stage/test.a"
    "$llvm/bin/clang" -std=c11 -Wall -Wextra -Werror -I "$stage" native_laws.c \
        "$stage/test.a" -Wl,--wrap=fcntl -lpthread -lm -o "$stage/native-laws"
    test_dir=$(mktemp -d "$test_root/sqlite-laws.XXXXXX")
    (cd "$test_dir" && ulimit -c 0 && "$stage/native-laws")
    exit
fi
cp hirsel_sqlite.c hirsel_sqlite_os.c hirsel_sqlite_os.h "$stage"
(
    cd "$stage"
    for file in sqlite3 hirsel_sqlite hirsel_sqlite_os; do
        "$llvm/bin/clang" --target=x86_64-unknown-linux-gnu -std=c11 -O2 \
            -fPIC -fno-ident "-ffile-prefix-map=$PWD=." \
            -DSQLITE_OS_OTHER=1 -DSQLITE_THREADSAFE=1 -DSQLITE_TEMP_STORE=3 \
            -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_DESERIALIZE -DSQLITE_OMIT_LOCALTIME \
            -DSQLITE_USE_URI=0 -DSQLITE_DQS=0 -DSQLITE_DEFAULT_FOREIGN_KEYS=1 \
            -c "$file.c" -o "$file.o"
    done
    "$llvm/bin/ld.lld" -r sqlite3.o hirsel_sqlite.o hirsel_sqlite_os.o -o hsq.o
    "$llvm/bin/llvm-objcopy" --wildcard --keep-global-symbol='hsq_*' hsq.o
    "$llvm/bin/llvm-ar" rcs libhirsel_sqlite.a hsq.o
)
cp "$stage/libhirsel_sqlite.a" libhirsel_sqlite.a
