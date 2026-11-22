# Hirsel Tree-sitter grammar

Run `editors/tree-sitter-hirsel/check-local` from the repository root to
build the parser and check the corpus. The command caches Tree-sitter CLI
0.25.10 and a Python environment with tree-sitter 0.25.2 in
`~/.cache/hirsel-tree-sitter/`. It needs a C compiler and Python with
`venv` support.

Run these development commands from this directory with Tree-sitter CLI
0.25.10 and `ts_query_ls` 3.16.0 installed.

```sh
tree-sitter generate
tree-sitter test
tree-sitter build -o "${TMPDIR:-/tmp}/hirsel.so"
ts_query_ls check -f queries/ --config "{\"parser_install_directories\":[\"${TMPDIR:-/tmp}\"]}"
cargo test --locked
npm ci
```

The corpus check parses every `.hirsel` file under `core/`, every Hirsel fence
in the spec, and the primer when present. Existing syntax refusal markers
require an error on the marked line. Semantic refusals must parse cleanly.
The editor parser accepts function signatures because core and foreign
modules use them and Tree-sitter does not read package manifests.
