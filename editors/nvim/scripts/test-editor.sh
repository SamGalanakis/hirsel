#!/bin/sh
set -eu

package=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
trap 'exit 1' HUP INT TERM

"$package/scripts/install-parser.sh" "$scratch/runtime"
for query in highlights.scm injections.scm locals.scm; do
    cmp "$package/../tree-sitter-hirsel/queries/$query" "$package/queries/hirsel/$query"
done

ln -s "$package/../vscode/test/mock-lsp.py" "$scratch/hirsel"
mkdir "$scratch/workspace"
printf '%s\n' 'pkg_v1.Workspace(members: [])' > "$scratch/workspace/hirsel.hcfg"
cmp "$package/../tree-sitter-hirsel/data/queries/hirsel_data/highlights.scm" "$package/queries/hirsel_data/highlights.scm"
printf '%s\n' 'fn  main() -> u8 { 0 }' > "$scratch/workspace/main.hirsel"
printf '%s\n' 'fn target() -> u8 { 0 }' > "$scratch/workspace/target.hirsel"

export PATH="$scratch:$PATH"
export HIRSEL_MOCK_LOG="$scratch/wire.jsonl"
export HIRSEL_NVIM_ADAPTER="$package"
export HIRSEL_NVIM_RUNTIME="$scratch/runtime"
export HIRSEL_TEST_WORKSPACE="$scratch/workspace"
nvim --headless -u NONE -i NONE -l "$package/test/editor.lua"
