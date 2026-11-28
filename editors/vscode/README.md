# Hirsel for VS Code

Install Hirsel on PATH, in `~/.local/bin`, or in `~/.cargo/bin`. The extension
searches those locations in order. Set `hirsel.executable` in your user settings
to use a specific binary. Changing the setting restarts `hirsel lsp`.

The Hirsel status bar item shows the compiler version and server state. Click it
to open the Hirsel output channel. Use **Hirsel: Restart Language Server** to
restart manually. If package analysis reports a missing or stale lockfile, run
`hirsel lock` in the package workspace. The editor refreshes diagnostics when the
lockfile or `hirsel.hcfg` changes. The extension opens `.hcfg` files as Hirsel
Config and provides schema diagnostics, hover, completion, and formatting.

## Build from source

With Node.js 22 or newer, run `npm ci` and `npm run build` in `editors/vscode`.
The grammar build needs Emscripten, Docker, or Podman. It compiles the source and data Tree-sitter grammars and stages their highlight
queries in `dist/grammar`. The build
bundles the runtime dependencies into `dist/extension.js` and copies the
Tree-sitter runtime WASM next to it. Run `npm run package` to build a `.vsix`.
To work on the extension, launch the directory as an extension development host
(`code --extensionDevelopmentPath=/path/to/hirsel/editors/vscode`).

Run `npm test` for the grammar and semantic token checks. After building, run
`npm run test:editor` with Python 3 on PATH for the stdio and format-on-save smoke
test. Run `HIRSEL_TEST_DRIVER=/path/to/hirsel npm run test:bundle` to exercise the
bundle and real server together in stock VS Code from a directory without
`node_modules`. On headless Linux, run the editor tests under `xvfb-run`.

## Format on save

Add this to your VS Code settings.

```json
{
  "[hirsel][hirsel-data]": {
    "editor.defaultFormatter": "hirsel.hirsel",
    "editor.formatOnSave": true
  }
}
```
