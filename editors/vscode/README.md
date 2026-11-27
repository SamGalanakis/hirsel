# Hirsel for VS Code

Install Hirsel and add the executable to PATH. You can also set
`hirsel.executable` to its absolute path in your user settings. The extension
runs `hirsel lsp` for local `.hirsel` files. Open the **Hirsel** output channel
for server messages.

## Build from source

In `editors/vscode`, run `npm ci` and `npm run build`. The grammar build needs
Emscripten, Docker, or Podman. It compiles the repository's Tree-sitter parser
and scanner and stages the shared highlight query in `dist/grammar`.
Launch the directory as an extension development host
(`code --extensionDevelopmentPath=/path/to/hirsel/editors/vscode`).

Run `npm test` for the grammar and semantic token checks. After building, run
`npm run test:editor` with Python 3 on PATH for the stdio and format-on-save
smoke test. On headless Linux, run it under `xvfb-run`.

## Format on save

Add this to your VS Code settings.

```json
{
  "[hirsel]": {
    "editor.defaultFormatter": "hirsel.hirsel",
    "editor.formatOnSave": true
  }
}
```
