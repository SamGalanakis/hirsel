#!/bin/sh
set -eu

package=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
grammar="$package/../tree-sitter-hirsel"
destination=${1:-"$package"}
mkdir -p "$destination/parser"
output="$destination/parser/hirsel.so"
temporary=$(mktemp "$output.XXXXXX")
trap 'rm -f "$temporary"' EXIT
trap 'exit 1' HUP INT TERM

shared=-shared
if [ "$(uname -s)" = Darwin ]; then
    shared=-dynamiclib
fi
"${CC:-cc}" "$shared" -fPIC -O2 -I"$grammar/src" \
    "$grammar/src/parser.c" "$grammar/src/scanner.c" -o "$temporary"
mv -f "$temporary" "$output"
