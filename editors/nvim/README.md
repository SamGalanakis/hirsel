# Hirsel for Neovim

Use Neovim 0.11 or later. Install Hirsel and put its executable on PATH.
From this checkout, build the common Tree-sitter grammar for your machine.

```sh
editors/nvim/scripts/install-parser.sh
```

Add the absolute path to `editors/nvim` to your runtimepath. A plugin manager
can add the same directory. The parser script accepts an alternate runtimepath
directory as its first argument.

```lua
vim.opt.runtimepath:prepend("/path/to/hirsel/editors/nvim")
vim.api.nvim_create_autocmd("FileType", {
  pattern = "hirsel",
  callback = function(args)
    vim.treesitter.start(args.buf, "hirsel")
  end,
})
vim.lsp.enable("hirsel")
```

Neovim looks for `hirsel.hcfg` when choosing the LSP project root.
Regenerate the runtime queries when the common queries change by copying
`highlights.scm`, `injections.scm`, and `locals.scm` from
`editors/tree-sitter-hirsel/queries/` to `queries/hirsel/`.

## Format on save

Add this to your configuration before opening Hirsel files. Each buffer gets
one save hook for its Hirsel client. Detaching that client removes the hook.

```lua
local attach_group = vim.api.nvim_create_augroup("HirselFormatOnSave", { clear = true })
vim.api.nvim_create_autocmd("LspAttach", {
  group = attach_group,
  callback = function(args)
    local client = vim.lsp.get_client_by_id(args.data.client_id)
    if not client or client.name ~= "hirsel" or not client:supports_method("textDocument/formatting", args.buf) then
      return
    end

    local group = vim.api.nvim_create_augroup("HirselFormatBuffer" .. args.buf, { clear = true })
    vim.api.nvim_create_autocmd("BufWritePre", {
      group = group,
      buffer = args.buf,
      callback = function(save)
        vim.lsp.buf.format({ bufnr = save.buf, id = client.id, async = false })
      end,
    })
    vim.api.nvim_create_autocmd("LspDetach", {
      group = group,
      buffer = args.buf,
      callback = function(detach)
        if detach.data.client_id == client.id then
          vim.api.nvim_del_augroup_by_id(group)
        end
      end,
    })
  end,
})
```

Run `scripts/test-editor.sh` with Neovim on PATH to check the parser, runtime
queries, and LSP integration against the scripted server.
