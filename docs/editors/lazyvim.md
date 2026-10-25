# LazyVim

In repo-root mode (the default), Neovim needs nothing from lazymsbuild: clangd finds
`compile_commands.json` and `.clangd` at the repository root by itself. This page adds the
conveniences: lazymsbuild in a floating terminal, the build's errors in the quickfix list, a
notification when a build ends, and `:CompileCommands`.

## 1. clangd

1. Enable LazyVim's clangd extra: `:LazyExtras`, then `lang.clangd` (it adds
   `"lazyvim.plugins.extras.lang.clangd"` to `lazyvim.json`). It installs clangd with Mason.
2. In the repository: `lazymsbuild init`, then `lazymsbuild sync` (or a rebuild from the TUI).
3. Open a `.cpp` file. `<leader>cl` (LSP info) shows clangd attached; go to definition (`gd`)
   and the diagnostics use the solution's include paths and defines.

## 2. The plugin file

Save this as `lua/plugins/lazymsbuild.lua`:

```lua
-- lazymsbuild for LazyVim (https://github.com/6r1ntry52o/lazymsbuild, docs/editors/lazyvim.md)
--
-- Options, set before lazy.nvim starts (for example in lua/config/options.lua):
--   vim.g.lazymsbuild_external = true        -- external mode: give clangd --compile-commands-dir
--   vim.g.lazymsbuild_restart_clangd = true  -- restart clangd when a build or sync ends
--   vim.g.lazymsbuild_lualine = true         -- show a running build in lualine
--   vim.g.lazymsbuild_install_hint = "..."   -- how to install it, shown when it is not on PATH

local M = {}

local function notify(message, level)
  vim.notify(message, level or vim.log.levels.INFO, { title = "lazymsbuild" })
end

--- Whether lazymsbuild is on PATH; says so when it is not (also when it was installed after
--- Neovim started: PATH is read at start).
local function installed()
  if vim.fn.executable("lazymsbuild") == 1 then
    return true
  end
  local hint = vim.g.lazymsbuild_install_hint or "install it, then start Neovim from a new terminal"
  notify("lazymsbuild is not on PATH: " .. hint, vim.log.levels.ERROR)
  return false
end

--- The root of the current buffer's project (LazyVim's), or the current directory.
local function root()
  if _G.LazyVim and LazyVim.root then
    return LazyVim.root()
  end
  return vim.uv.cwd()
end

--- `lazymsbuild path <flag>` for the project `dir` is in. With `callback`, without waiting.
function M.path(flag, dir, callback)
  local cmd = { "lazymsbuild", "-C", dir or root(), "path", flag }
  local function value(res)
    return res.code == 0 and vim.trim(res.stdout) or nil
  end
  if vim.fn.executable("lazymsbuild") == 0 then
    if callback then
      callback(nil)
    end
    return nil
  end
  if callback then
    vim.system(cmd, { text = true }, function(res)
      vim.schedule(function()
        callback(value(res))
      end)
    end)
    return nil
  end
  return value(vim.system(cmd, { text = true }):wait())
end

--- :CompileCommands: fill the database without building.
function M.sync()
  notify("sync …")
  vim.system({ "lazymsbuild", "-C", root(), "sync", "-q" }, { text = true }, function(res)
    vim.schedule(function()
      local text = vim.trim((res.stdout or "") .. "\n" .. (res.stderr or ""))
      notify(text:sub(-800), res.code == 0 and vim.log.levels.INFO or vim.log.levels.ERROR)
    end)
  end)
end

--- The last build's errors and warnings in the quickfix list.
function M.errors()
  if not installed() then
    return
  end
  local state = M.path("--state")
  local file = state and vim.fs.joinpath(state, "errors.txt")
  if not (file and vim.uv.fs_stat(file)) then
    return notify("no build errors yet", vim.log.levels.WARN)
  end
  vim.fn.setqflist({}, " ", {
    title = "lazymsbuild",
    lines = vim.fn.readfile(file),
    -- MSVC format, file(line,col): error C2065: message; lines without a place stay as text.
    efm = [[%f(%l\,%c): %t%*\D%n: %m,%f(%l): %t%*\D%n: %m,%+G%.%#]],
  })
  vim.cmd("botright copen")
end

local function read_status(state)
  local ok, lines = pcall(vim.fn.readfile, vim.fs.joinpath(state, "status.json"))
  if not ok then
    return nil
  end
  local decoded, status = pcall(vim.json.decode, table.concat(lines, "\n"))
  return decoded and status or nil
end

local running = { building = true, syncing = true }

--- status.json watchers, by state directory.
M.watchers = {}

--- Watches the status.json of the project `dir` is in, and says when a build or sync ends,
--- also when the lazymsbuild terminal is hidden.
function M.watch(dir)
  M.path("--state", dir, function(state)
    if not state or M.watchers[state] then
      return
    end
    vim.fn.mkdir(state, "p")
    local watcher = { handle = vim.uv.new_fs_event(), status = read_status(state) }
    M.watchers[state] = watcher
    -- status.json is replaced as a whole, so the directory is watched, not the file.
    watcher.handle:start(state, {}, vim.schedule_wrap(function(err, name)
      if err or name ~= "status.json" then
        return
      end
      local status = read_status(state)
      if not status then
        return
      end
      local was = watcher.status
      watcher.status = status
      M.last = watcher
      -- Ended: it was seen running, or a new run (another start time) already finished in
      -- between, which a short build or a busy Neovim can make happen.
      local ended = not running[status.state]
        and (not was or running[was.state] or was.started_at ~= status.started_at)
      if ended then
        local level = status.state == "succeeded" and vim.log.levels.INFO or vim.log.levels.WARN
        notify(
          ("%s %s %s (E:%d W:%d)"):format(
            status.verb or "Build",
            status.profile or "",
            status.state,
            status.errors or 0,
            status.warnings or 0
          ),
          level
        )
        if vim.g.lazymsbuild_restart_clangd and #vim.lsp.get_clients({ name = "clangd" }) > 0 then
          vim.cmd("LspRestart clangd")
        end
      end
    end))
  end)
end

--- For lualine: the running build, or the errors of a failed one; "" otherwise.
function M.statusline()
  local status = M.last and M.last.status
  if not status then
    return ""
  end
  if running[status.state] then
    return ("%s %s…"):format(status.verb or "Build", status.profile or "")
  elseif status.state == "failed" then
    return ("✘ E:%d"):format(status.errors or 0)
  end
  return ""
end

--- What `e` on an error in the TUI runs: hide the floating terminal, then open the file in this
--- Neovim (not in a second one inside the terminal).
function M.editor()
  local keys = [[<C-\><C-N>:close<CR>:execute 'edit +{line} ' .. fnameescape('{file}')<CR>]]
  -- --headless: a client without it writes terminal queries into lazymsbuild's terminal.
  return ('nvim --headless --server %s --remote-send "%s"'):format(vim.v.servername, keys)
end

--- lazymsbuild in a floating terminal, like lazygit's <leader>gg. Hiding it keeps the build going.
function M.toggle()
  if not installed() then
    return
  end
  Snacks.terminal.toggle("lazymsbuild", { cwd = root(), env = { LAZYMSBUILD_EDITOR = M.editor() } })
end

package.loaded["lazymsbuild"] = M

return {
  {
    "neovim/nvim-lspconfig",
    optional = true,
    opts = function(_, opts)
      if not vim.g.lazymsbuild_external then
        return
      end
      local dir = M.path("--db")
      if not dir then
        return
      end
      opts.servers = opts.servers or {}
      opts.servers.clangd = opts.servers.clangd or {}
      -- Keep LazyVim's clangd flags; add the database's directory.
      local cmd = vim.deepcopy(opts.servers.clangd.cmd or { "clangd" })
      table.insert(cmd, "--compile-commands-dir=" .. dir)
      opts.servers.clangd.cmd = cmd
    end,
  },
  {
    "folke/snacks.nvim",
    init = function()
      -- Without lazymsbuild, leave :CompileCommands to any older setup that defines it.
      if vim.fn.executable("lazymsbuild") == 1 then
        vim.api.nvim_create_user_command("CompileCommands", M.sync, { desc = "lazymsbuild sync (compile_commands.json)" })
      end
      vim.api.nvim_create_autocmd("FileType", {
        group = vim.api.nvim_create_augroup("lazymsbuild", { clear = true }),
        pattern = { "c", "cpp" },
        callback = function(event)
          local name = vim.api.nvim_buf_get_name(event.buf)
          if name ~= "" then
            M.watch(vim.fs.dirname(name))
          end
        end,
      })
    end,
    keys = {
      { "<leader>mb", M.toggle, desc = "lazymsbuild" },
      { "<leader>me", M.errors, desc = "lazymsbuild errors → quickfix" },
    },
  },
  { "folke/which-key.nvim", optional = true, opts = { spec = { { "<leader>m", group = "msbuild" } } } },
  {
    "nvim-lualine/lualine.nvim",
    optional = true,
    opts = function(_, opts)
      if vim.g.lazymsbuild_lualine then
        table.insert(opts.sections.lualine_x, 1, { M.statusline })
      end
    end,
  },
}
```

| Key / command | |
|---|---|
| `<leader>mb` | lazymsbuild in a floating terminal for the current project. The same key hides it; the build goes on |
| `<leader>me` | The last build's errors and warnings in the quickfix list (`errors.txt`) |
| `:CompileCommands` | `lazymsbuild sync`: fill the database without building |
| (automatic) | A notification when a build or sync ends, also with the terminal hidden |
| `e` in the TUI's Errors tab | Hides the terminal and opens the file at the error in this Neovim |

## When clangd picks up a new database

clangd does not watch `compile_commands.json`; it reads it again the next time it needs a compile
command. A buffer that is open picks up the new flags at its next edit, a file opened afterwards at
once. That is usually enough. To restart clangd whenever a build or sync ends, set
`vim.g.lazymsbuild_restart_clangd = true` (a restart re-parses every open file).

## External mode

With `lazymsbuild init --mode external`, the database is outside the repository and clangd has to
be told where. Set `vim.g.lazymsbuild_external = true`: the plugin asks `lazymsbuild path --db`
when the LSP starts and adds `--compile-commands-dir`. The directory is taken from where Neovim was
started, so this serves one project per Neovim. For clangd's settings in external mode, add the
block `lazymsbuild init --mode external` prints to `%LOCALAPPDATA%\clangd\config.yaml`.

## Notes

- `lazymsbuild` must be on `PATH` when Neovim starts; without it the plugin stays out of the way
  (no `:CompileCommands`, no notifications), and `<leader>mb` / `<leader>me` only say that it is
  not on `PATH` (with `vim.g.lazymsbuild_install_hint`, if set, telling how to install it).
- A repository with several solutions: run a command with `--solution <file>` once (for example
  `lazymsbuild --solution <file> init`); the plugin's calls then use that solution.
- Coming from a setup that wrote `build/compile_commands.json` (CMake habits, an older
  `:CompileCommands`): delete that file, or clangd may keep reading it; `lazymsbuild check` lists
  such files under "other databases".
- Without Snacks (an older LazyVim), replace `M.toggle` with any terminal plugin running
  `lazymsbuild` in the project directory.
