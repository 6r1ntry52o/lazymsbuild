-- Runs in a headless Neovim with LazyVim and docs/editors/lazyvim.md's plugin file (see
-- lazyvim.ps1). Prints one line per check and exits 0 when all pass.

local repo = assert(os.getenv("LAZYMSBUILD_TEST_REPO"), "LAZYMSBUILD_TEST_REPO")
local failures = {}

local function out(text)
  io.stdout:write(text .. "\n")
  io.stdout:flush()
end

local function check(name, ok, detail)
  out(("%s %-24s %s"):format(ok and "ok  " or "FAIL", name, detail and tostring(detail) or ""))
  if not ok then
    table.insert(failures, name)
  end
end

local notes = {}
vim.notify = function(message)
  table.insert(notes, tostring(message))
end
local function noted(pattern)
  for _, n in ipairs(notes) do
    if n:find(pattern) then
      return true
    end
  end
  return false
end

local function wait(ms, condition)
  return vim.wait(ms, condition, 50)
end

-- The plugin file
local loaded, M = pcall(require, "lazymsbuild")
check("plugin module", loaded and type(M) == "table")
check(":CompileCommands", vim.api.nvim_get_commands({})["CompileCommands"] ~= nil)
check("<leader>mb", vim.fn.maparg("<leader>mb", "n") ~= "")
check("<leader>me", vim.fn.maparg("<leader>me", "n") ~= "")

-- A C++ file: clangd attaches with the database, the watcher starts
vim.cmd.edit(vim.fs.joinpath(repo, "src", "App", "main.cpp"))
local buf = vim.api.nvim_get_current_buf()
check("filetype", vim.bo[buf].filetype == "cpp", vim.bo[buf].filetype)
check("clangd attached", wait(90000, function()
  return #vim.lsp.get_clients({ name = "clangd", bufnr = buf }) > 0
end))
check("status.json watched", wait(15000, function()
  return next(M.watchers) ~= nil
end))

-- Diagnostics: none, since the include paths and defines come from the database
local published = false
vim.api.nvim_create_autocmd("DiagnosticChanged", {
  buffer = buf,
  callback = function()
    published = true
  end,
})
vim.api.nvim_buf_set_lines(buf, -1, -1, false, { "" }) -- an edit makes clangd publish again
wait(30000, function()
  return published
end)
local errors = vim.diagnostic.get(buf, { severity = vim.diagnostic.severity.ERROR })
check("no errors in main.cpp", published and #errors == 0, #errors > 0 and errors[1].message or nil)
vim.cmd("silent! undo")

-- Go to definition: core::answer() in MyApp.Core (core.cpp once indexed, core.h before)
local line, column
for i, text in ipairs(vim.api.nvim_buf_get_lines(buf, 0, -1, false)) do
  local found = text:find("answer")
  if found then
    line, column = i - 1, found - 1
    break
  end
end
local target
wait(60000, function()
  local params = { textDocument = vim.lsp.util.make_text_document_params(buf), position = { line = line, character = column } }
  local results = vim.lsp.buf_request_sync(buf, "textDocument/definition", params, 5000) or {}
  for _, response in pairs(results) do
    local result = response.result
    local location = result and (result[1] or result)
    if location and (location.uri or location.targetUri) then
      target = vim.uri_to_fname(location.uri or location.targetUri)
      if target:find("core%.cpp$") then
        return true
      end
    end
  end
  return false
end)
check("go to definition", target and (target:find("core%.cpp$") or target:find("core%.h$")), target)

-- A build started outside Neovim: the watcher reports its end
notes = {}
vim.system({ "lazymsbuild", "-C", repo, "build", "-q" }):wait()
check("build notification", wait(20000, function()
  return noted("Build Debug|x64 succeeded")
end), table.concat(notes, " / "))

-- A failing build: its errors in the quickfix list
local core = vim.fs.joinpath(repo, "src", "Core", "core.cpp")
local original = vim.fn.readfile(core)
vim.fn.writefile({ "int broken() { return undeclared_name; }" }, core)
vim.system({ "lazymsbuild", "-C", repo, "build", "-q" }):wait()
vim.fn.writefile(original, core)
M.errors()
local item
for _, entry in ipairs(vim.fn.getqflist()) do
  if entry.valid == 1 and entry.lnum == 1 and vim.api.nvim_buf_get_name(entry.bufnr):find("core%.cpp$") then
    item = entry
  end
end
check("errors in quickfix", item ~= nil, #vim.fn.getqflist() .. " items")
check("quickfix error type", item and item.type == "e" and item.nr == 2065, item and (item.type .. " " .. item.nr))

-- :CompileCommands runs lazymsbuild sync
notes = {}
vim.cmd("CompileCommands")
check("sync via :CompileCommands", wait(120000, function()
  return noted("Sync succeeded") or noted("design%-time extractor")
end), table.concat(notes, " / "))

-- The editor command the TUI's `e` uses inside the floating terminal
local editor = M.editor()
check("editor command", editor:find("^nvim %-%-headless %-%-server ") and editor:find("{file}", 1, true) and editor:find("{line}", 1, true), editor)

-- The keys `e` sends, with the placeholders filled as the TUI fills them. A script cannot receive
-- --remote-send input while it runs (typed input waits for the command to end), so the keys are
-- fed directly; the quickfix window, current after M.errors(), stands in for the terminal float.
local file = vim.fs.joinpath(repo, "src", "Core", "include", "core.h")
local keys = editor:match('"(.*)"$'):gsub("{line}", "3"):gsub("{file}", function()
  return file
end)
local windows = #vim.api.nvim_list_wins()
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(keys, true, false, true), "x", false)
check("e opens the error here", vim.fs.normalize(vim.api.nvim_buf_get_name(0)) == vim.fs.normalize(file)
  and vim.fn.line(".") == 3 and #vim.api.nvim_list_wins() == windows - 1, vim.api.nvim_buf_get_name(0) .. ":" .. vim.fn.line("."))

-- Not on PATH (also: installed after Neovim started): the keys say so instead of failing in a terminal
local path = vim.env.PATH
local exe = vim.fn.exepath("lazymsbuild")
local dir = vim.fs.normalize(vim.fs.dirname(exe)):lower()
vim.env.PATH = table.concat(vim.tbl_filter(function(entry)
  return vim.fs.normalize(entry):lower() ~= dir
end, vim.split(path, ";", { plain = true })), ";")
local own_hint = vim.g.lazymsbuild_install_hint -- a configuration may set its own
vim.g.lazymsbuild_install_hint = nil
notes = {}
windows = #vim.api.nvim_list_wins()
M.toggle()
check("toggle: not on PATH", noted("not on PATH: install it, then") and #vim.api.nvim_list_wins() == windows, table.concat(notes, " / "))
notes = {}
vim.g.lazymsbuild_install_hint = "run install.ps1"
M.errors()
check("errors: install hint", noted("not on PATH: run install%.ps1$"), table.concat(notes, " / "))
vim.g.lazymsbuild_install_hint = own_hint
vim.env.PATH = path

if #failures == 0 then
  out("LAZYVIM CHECK OK")
  vim.cmd("qa!")
else
  out("LAZYVIM CHECK FAILED: " .. table.concat(failures, ", "))
  vim.cmd("cq")
end
