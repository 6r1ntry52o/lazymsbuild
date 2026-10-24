# Configuration

lazymsbuild works without any configuration. Settings come, from weakest to strongest, from:

1. the built-in defaults below;
2. your `config.toml`: the first of `%LAZYMSBUILD_CONFIG_DIR%\config.toml`,
   `%XDG_CONFIG_HOME%\lazymsbuild\config.toml`, `%APPDATA%\lazymsbuild\config.toml` that exists
   (`lazymsbuild path --config` prints the directory);
3. the repository's `.lazymsbuild.toml` at its root, which a team can commit;
4. command-line options (`--profile`, `--no-parallel`, ...).

Tables merge key by key, so a repository file only has to name what it changes. A key
lazymsbuild does not know is reported as a warning and ignored, never an error, so a newer file
still works with an older lazymsbuild. `schema/config.schema.json` describes the format for editor
completion: with [taplo](https://taplo.tamasfe.dev/) (also in VS Code's *Even Better TOML*), start the
file with

```toml
#:schema https://raw.githubusercontent.com/6r1ntry52o/lazymsbuild/main/schema/config.schema.json
```

## Example

```toml
schema_version = 1

[build]
parallel = true
extra_args = ["-p:TreatWarningsAsErrors=false"]

[output]
path = "compile_commands.json"

[keys]
build = ["b", "f7"]
```

## Keys

### Top level

| Key | Type | Default | |
|---|---|---|---|
| `schema_version` | integer | `1` | The version of this format; a newer number only adds a warning |
| `solution` | string | | The solution to use, relative to the repository root, when there are several (usually in `.lazymsbuild.toml`) |
| `profile` | string | | The configuration to start with (`"Release\|x64"`). Otherwise: the one `msbuild-extractor.json` declares, then the solution's first |

### `[build]`

| Key | Type | Default | |
|---|---|---|---|
| `msbuild` | path | | MSBuild.exe. Empty: the newest Visual Studio or Build Tools with the C++ build tools, found with vswhere (preferring the solution's Visual Studio version; the 64-bit MSBuild for 2022 and later) |
| `parallel` | boolean | `true` | Build projects in parallel (`-m`). The TUI's `m` and `--no-parallel` change it for a run |
| `max_cpu_count` | integer | `0` | `-m:<n>`; `0` lets MSBuild decide |
| `verbosity` | string | `"minimal"` | MSBuild's console verbosity in the log: `quiet`, `minimal`, `normal`, `detailed`, `diagnostic` |
| `extra_args` | array of strings | `[]` | Added to every MSBuild command line. No argument may contain `"` (MSBuild does not read `\"`) |
| `english` | boolean | `true` | English messages: `DOTNET_CLI_UI_LANGUAGE=en-US` for MSBuild, `VSLANG=1033` for the compiler and linker (which use it only when their English resources are installed). Errors are found by their codes either way |
| `keep_binlog` | boolean | `false` | Also write a binary log (`-bl`) next to each build log, for MSBuild Structured Log Viewer |
| `code_page` | integer | `0` | Code page of build output that is not UTF-8; `0` is the OEM code page, which MSBuild writes in |

### `[extractor]`

| Key | Type | Default | |
|---|---|---|---|
| `design_time` | string | | msbuild-extractor-sample.exe for `sync`. Empty: `PATH`, then `<repository>\.tools\`, then `%LOCALAPPDATA%\msbuild-extractor\`. `"off"`: never (sync is unavailable; a rebuild fills the database) |
| `logger` | string | | LazyMsBuild.Logger.dll. Empty: next to lazymsbuild.exe. `"off"`: builds do not update the database |

### `[output]`

| Key | Type | Default | |
|---|---|---|---|
| `mode` | string | `"repo-root"` | `repo-root`: the database goes into the repository, where clang tools find it. `external`: into the state directory; only editors given `--compile-commands-dir` use it. `lazymsbuild init --mode` sets it for one repository |
| `path` | path | `"compile_commands.json"` | In repo-root mode, relative to the repository root. `"build/compile_commands.json"` matches CMake habits; clangd finds it there too, clang-tidy then needs `-p build` |
| `format` | string | `"arguments"` | `arguments` (recommended by the specification) or `command`, for tools that read only `command` |

### `[init]`

| Key | Type | Default | |
|---|---|---|---|
| `ignore` | string | `"exclude"` | How `init` keeps generated files out of Git: `exclude` (`.git/info/exclude`, local), `gitignore` (asks first), `none` |

### `[clangd]`

| Key | Type | Default | |
|---|---|---|---|
| `path` | path | | clangd.exe for `doctor`. Empty: `PATH`, Mason's (Neovim), Visual Studio's LLVM, the VS Code extension's |

### `[logs]`

| Key | Type | Default | |
|---|---|---|---|
| `keep` | integer | `20` | How many build logs to keep in the state directory |

### `[ui]`

| Key | Type | Default | |
|---|---|---|---|
| `edit_at_line` | string | | The command `e` runs for an error, with `{file}`, `{line}` and `{column}`: `"code -g {file}:{line}:{column}"`. Empty: `$EDITOR`, then `$VISUAL` (common editors get the line: `nvim +{line}`, `code -g`, ...), then the default application |

### `[keys]`

Each TUI action takes a key or a list of keys, which replace its defaults. Keys are written `b`,
`G` (Shift is in the case), `ctrl+u`, `shift+tab`, `enter`, `space`, `esc`, `up`, `pagedown`,
`f5`, ... A key given to another action is taken from its default action.

| Action | Default | |
|---|---|---|
| `pane_1` … `pane_3` | `1` … `3` | Go to a pane on the left |
| `focus_main` | `0` | Go to the log (`esc`, `tab`, `h` and `l` go back) |
| `next_pane`, `prev_pane` | `tab` `l`, `shift+tab` `h` | Cycle the panes on the left |
| `up`, `down` | `k` `up`, `j` `down` | Move, scroll |
| `top`, `bottom` | `g` `<` `home`, `G` `>` `end` | First, last (in the log: back to following the output) |
| `page_up`, `page_down` | `,`, `.` | A page |
| `scroll_main_up`, `scroll_main_down` | `K` `pageup` `ctrl+u`, `J` `pagedown` `ctrl+d` | Scroll the log half a page from any pane |
| `prev_tab`, `next_tab` | `[`, `]` | Log / Errors |
| `select` | `enter` `space` | Select the target or configuration; run the action |
| `filter` | `/` | Filter the targets |
| `help` | `?` | All keys |
| `menu` | `x` | All actions |
| `build`, `rebuild`, `clean` | `b`, `r`, `c` | Clean asks first |
| `sync` | `s` | Fill the database without building |
| `parallel` | `m` | `-m` on/off |
| `edit` | `e` | Open the selected error |
| `cancel` | `ctrl+c` | Cancel the build (asks first); quit when idle |
| `close` | `esc` | Close the filter or a menu; leave the log |
| `quit` | `q` | Quit (asks first during a build) |

## Environment variables

| Variable | |
|---|---|
| `LAZYMSBUILD_CONFIG_DIR` | The user configuration directory |
| `XDG_CONFIG_HOME` | Then `%XDG_CONFIG_HOME%\lazymsbuild` |
| `LAZYMSBUILD_STATE_DIR` | The state root instead of `%LOCALAPPDATA%\lazymsbuild` (tests, portable setups) |
| `LAZYMSBUILD_EDITOR` | Like `ui.edit_at_line`, and stronger: for editor integrations that start lazymsbuild in their own terminal (the LazyVim one does) |
| `EDITOR`, `VISUAL` | What `e` opens, without an `edit_at_line` |
| `NO_COLOR` | No colors (<https://no-color.org>) |
