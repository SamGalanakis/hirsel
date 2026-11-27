local adapter = vim.env.HIRSEL_NVIM_ADAPTER
local workspace = vim.env.HIRSEL_TEST_WORKSPACE
local main = workspace .. "/main.hirsel"
vim.opt.runtimepath:prepend(adapter)
vim.opt.runtimepath:prepend(vim.env.HIRSEL_NVIM_RUNTIME)
vim.cmd.filetype("on")

local readme = table.concat(vim.fn.readfile(adapter .. "/README.md"), "\n")
local start = assert(readme:find("local attach_group", 1, true))
local finish = assert(readme:find("\n```", start, true))
assert(load(readme:sub(start, finish - 1), "@README.md"))()

vim.lsp.enable("hirsel")
vim.cmd.edit(main)
assert(vim.bo.filetype == "hirsel")
local bufnr = vim.api.nvim_get_current_buf()

vim.treesitter.start(bufnr, "hirsel")
local tree = vim.treesitter.get_parser(bufnr):parse()[1]
assert(not tree:root():has_error())
local query = assert(vim.treesitter.query.get("hirsel", "highlights"))
local saw_keyword = false
for id, node in query:iter_captures(tree:root(), bufnr) do
  if query.captures[id] == "keyword" and vim.treesitter.get_node_text(node, bufnr) == "fn" then
    saw_keyword = true
  end
end
assert(saw_keyword, "missing Tree-sitter keyword highlight")
for _, name in ipairs({ "injections", "locals" }) do
  assert(vim.treesitter.query.get("hirsel", name))
end

local client
assert(vim.wait(10000, function()
  client = vim.lsp.get_clients({ name = "hirsel", bufnr = bufnr })[1]
  return client ~= nil
end), "Hirsel client did not attach")
assert(vim.deep_equal(client.config.cmd, { "hirsel", "lsp" }))
assert(client.config.root_dir == workspace, client.config.root_dir)

vim.api.nvim_buf_set_lines(bufnr, 0, 1, false, { "fn  main() -> u8 { 1 }" })
assert(vim.wait(5000, function()
  return vim.diagnostic.get(bufnr)[1] ~= nil
end), "missing diagnostic after didChange")
assert(vim.diagnostic.get(bufnr)[1].code == "MOCK-1")

local function request(method)
  local responses = assert(vim.lsp.buf_request_sync(
    bufnr, method, vim.lsp.util.make_position_params(0, client.offset_encoding), 5000
  ))
  local response = assert(responses[client.id])
  assert(not response.err, vim.inspect(response.err))
  return assert(response.result)
end

assert(request("textDocument/hover").contents.value == "`u8`")
assert(request("textDocument/definition").uri == vim.uri_from_fname(workspace .. "/target.hirsel"))

vim.cmd.write()
assert(vim.fn.readfile(main)[1] == "fn main() -> u8 { 1 }")
vim.api.nvim_buf_set_lines(bufnr, 0, 1, false, { "fn main() -> u8 { 2 }" })
vim.cmd.write()
assert(vim.fn.readfile(main)[1] == "fn main() -> u8 { 2 }")

local log = vim.fn.readfile(vim.env.HIRSEL_MOCK_LOG)
assert(vim.deep_equal(vim.json.decode(log[1]).argv, { "hirsel", "lsp" }))
local methods = {}
for index = 2, #log do
  local message = vim.json.decode(log[index])
  methods[message.method] = (methods[message.method] or 0) + 1
end
assert(methods.initialize == 1)
for _, method in ipairs({ "didOpen", "didChange", "hover", "definition" }) do
  assert(methods["textDocument/" .. method], method)
end
assert(methods["textDocument/formatting"] == 2, "expected one format request per save")

vim.lsp.buf_detach_client(bufnr, client.id)
assert(not pcall(vim.api.nvim_get_autocmds, { group = "HirselFormatBuffer" .. bufnr }),
  "format-on-save group survived detach")
print("Neovim parser, queries, stdio, two format-on-save operations, and detach passed")
