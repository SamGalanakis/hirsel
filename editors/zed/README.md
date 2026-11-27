# Hirsel for Zed

Install Hirsel and add the executable to PATH. For a checkout, run
`zed: install dev extension` from Zed's command palette and select
`editors/zed`. Zed builds the extension with Rust and fetches and builds the
Tree-sitter grammar from the revision in `extension.toml`.

To format Hirsel buffers when you save, add this to your Zed settings.

```json
{
  "languages": {
    "Hirsel": {
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
