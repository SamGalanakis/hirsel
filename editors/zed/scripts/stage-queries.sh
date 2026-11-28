#!/bin/sh
set -eu

package=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
grammar="$package/../tree-sitter-hirsel"
source_dir="$grammar/queries"
destination="$package/languages/hirsel"

case "${1:-}" in
    '') mode=cp ;;
    --check) mode=cmp ;;
    *) echo "Usage: $0 [--check]" >&2; exit 2 ;;
esac

"$mode" "$source_dir/highlights.scm" "$destination/highlights.scm"
"$mode" "$grammar/data/queries/hirsel_data/highlights.scm" "$package/languages/hirsel-data/highlights.scm"
# Zed requires an injection.content capture when an injections query exists.
if grep -q '^[[:space:]]*[^;[:space:]]' "$source_dir/injections.scm"; then
    "$mode" "$source_dir/injections.scm" "$destination/injections.scm"
elif [ "$mode" = cp ]; then
    rm -f "$destination/injections.scm"
elif [ -e "$destination/injections.scm" ]; then
    echo "Zed must omit the empty Hirsel injections query." >&2
    exit 1
fi
