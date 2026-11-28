return {
  cmd = { "hirsel", "lsp" },
  filetypes = { "hirsel", "hirsel_data" },
  root_markers = { "hirsel.hcfg" },
  capabilities = {
    workspace = { didChangeWatchedFiles = { dynamicRegistration = true } },
  },
}
