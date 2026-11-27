{
  "targets": [
    {
      "target_name": "tree_sitter_hirsel_binding",
      "dependencies": ["<!(node -p \"require('node-addon-api').targets\"):node_addon_api_except"],
      "include_dirs": ["src"],
      "sources": ["bindings/node/binding.cc", "src/parser.c", "src/scanner.c", "data/src/parser.c", "data/src/scanner.c"],
      "conditions": [
        ["OS!='win'", { "cflags_c": ["-std=c11"] }],
        ["OS=='win'", { "msvs_settings": { "VCCLCompilerTool": { "AdditionalOptions": ["/std:c11"] } } }]
      ]
    }
  ]
}
