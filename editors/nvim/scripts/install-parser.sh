#!/bin/sh
set -eu

package=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
grammar="$package/../tree-sitter-hirsel"
destination=${1:-"$package"}
mkdir -p "$destination/parser"
shared=-shared
if [ "$(uname -s)" = Darwin ]; then
    shared=-dynamiclib
fi

install_parser() {
    source_dir=$1
    output="$destination/parser/$2.so"
    temporary=$(mktemp "$output.XXXXXX")
    trap 'rm -f "$temporary"' EXIT
    trap 'exit 1' HUP INT TERM
    "${CC:-cc}" "$shared" -fPIC -O2 -I"$source_dir" \
        "$source_dir/parser.c" "$source_dir/scanner.c" -o "$temporary"
    mv -f "$temporary" "$output"
}

install_parser "$grammar/src" hirsel
install_parser "$grammar/data/src" hirsel_data
