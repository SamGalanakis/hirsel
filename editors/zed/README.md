# Hirsel for Zed

Install Hirsel and add the executable to PATH. For a checkout, run
`zed: install dev extension` from Zed's command palette and select
`editors/zed`. Zed builds the extension with Rust and fetches and builds the
source and configuration Tree-sitter grammars from the revisions in
`extension.toml`. Zed opens `.hcfg` files as Hirsel Config and uses the Hirsel
server for diagnostics, hover, completion, and formatting.

To format Hirsel buffers when you save, add this to your Zed settings.

```json
{
  "languages": {
    "Hirsel": {
      "formatter": "language_server",
      "format_on_save": "on"
    },
    "Hirsel Config": {
      "formatter": "language_server",
      "format_on_save": "on"
    }
  }
}
```

Run `scripts/stage-queries.sh` when the common highlights or injections change,
and update the pinned grammar revision in `extension.toml` to the matching
published commit. Use `scripts/stage-queries.sh --check` to compare the staged
queries with the common files. Zed requires an injection capture when a query
file exists, so the script omits the common comment-only injections file.
