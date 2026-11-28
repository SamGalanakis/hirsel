#!/bin/sh
set -eu

package=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
source_dir="$package/../tree-sitter-hirsel/queries"
destination="$package/languages/hirsel"

case "${1:-}" in
    '') mode=cp ;;
    --check) mode=cmp ;;
    *) echo "Usage: $0 [--check]" >&2; exit 2 ;;
esac

"$mode" "$source_dir/highlights.scm" "$destination/highlights.scm"
"$mode" "$source_dir/data-highlights.scm" "$package/languages/hirsel-data/highlights.scm"
# Zed requires an injection.content capture when an injections query exists.
if grep -q '^[[:space:]]*[^;[:space:]]' "$source_dir/injections.scm"; then
    "$mode" "$source_dir/injections.scm" "$destination/injections.scm"
elif [ "$mode" = cp ]; then
    rm -f "$destination/injections.scm"
elif [ -e "$destination/injections.scm" ]; then
    echo "Zed must omit the empty Hirsel injections query." >&2
    exit 1
fi
