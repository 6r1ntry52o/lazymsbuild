# lazymsbuild

A [lazygit](https://github.com/jesseduffield/lazygit)-style terminal UI for MSBuild that keeps
`compile_commands.json` complete, so that clangd (and clang-tidy, and every editor that uses them)
understands a Visual Studio C++ solution: its include paths, its defines, all of its files.

```text
+-[1] Solution & Targets ------------+-[4] Log | Errors (E:0 W:3) --------------+
| > [Solution] MySolution.sln        |   core.cpp                                |
|   ├─ Libs/                         |   MyApp.Core.vcxproj -> ...\MyApp.Core.lib|
|   │   └─ CoreEngine                | Build succeeded.                          |
|   ├─ ControlApp                    |                                           |
+-[2] Configuration / Platform ------+                                           |
| * Debug   | x64                    |                                           |
|   Release | x64                    |                                           |
+-[3] Actions & Options -------------+-[5] clangd DB Status ---------------------+
| b Build  r Rebuild  c Clean        | Profile  : Debug|x64                      |
| s Sync   m Parallel (-m) : ON      | Entries  : 342 (+2)   Coverage: 342/350   |
|                                    | Stale    : none       Updated : 12:03:11  |
+------------------------------------+-------------------------------------------+
 b build · r rebuild · s sync · x menu · ? help
```

- **Builds like Visual Studio.** The solution is always the entry point, so the solution's
  configuration mapping applies; a single project is built through its solution target.
  Visual Studio 2022's 64-bit MSBuild and compiler are used, as the IDE does.
- **A complete database without building.** `lazymsbuild sync` fills it from a design-time
  evaluation of every project. After that, every build refreshes the entries of the files it
  compiled, with the exact command lines MSBuild ran.
- **Found where clang tools look.** `compile_commands.json` goes to the repository root, where
  clangd and clang-tidy find it by themselves, and it never shows up in `git status`.
- **Answers "is it up to date?".** `lazymsbuild check` reports coverage, stale projects and
  anything clangd would read instead; `lazymsbuild doctor` checks the whole path to clangd.
- **The build runs on its own.** Its state is in `status.json`, so an editor can show it and
  notify you when the build ends, whether the TUI is visible or hidden.
- **Nothing leaves your machine.** No telemetry, no network access, no automatic updates.

## Requirements

- Windows 10 or 11 (x64 or ARM64)
- Visual Studio 2019 or later, or the Build Tools, with *Desktop development with C++*
- C++ projects (`.vcxproj`) in a `.sln` or `.slnx`; x86, x64 and ARM64 platforms
- For `sync`: Microsoft's [msbuild-extractor-sample](https://github.com/microsoft/msbuild-extractor-sample)
  (`scripts/install-extractor.ps1` installs it). Without it, a rebuild fills the database instead.
- clangd, to use the result

## Install

- **GitHub Releases**: download `lazymsbuild-v<version>-x86_64-pc-windows-msvc.zip`, check it
  against `SHA256SUMS`, and put `lazymsbuild.exe` and `LazyMsBuild.Logger.dll` (it must stay next
  to the exe) in a directory on your `PATH`.
- **cargo**: `cargo binstall lazymsbuild` installs the release archive. `cargo install lazymsbuild`
  builds the exe only; build the logger with `dotnet build logger -c Release` from a clone and
  point `extractor.logger` at `LazyMsBuild.Logger.dll` (see `docs/config.md`).
- **[dotfiles-windows](https://github.com/6r1ntry52o/dotfiles-windows)**: `install.ps1` installs a
  pinned release and sets up Neovim.

## Getting started

In the repository that has your solution:

1. `lazymsbuild init` writes a `.clangd` (unless there is one) and keeps the generated files out
   of Git (`.git/info/exclude`; `--ignore gitignore` uses `.gitignore` instead, after asking).
2. `lazymsbuild sync` fills the database for the active configuration without building. Without
   the design-time extractor, start the TUI (`lazymsbuild`) and rebuild the whole solution once.
3. `lazymsbuild check` should report full coverage and no stale project.
4. Turn on clangd in your editor. Nothing else to configure: clangd finds the database at the
   root. See [`docs/editors/`](docs/editors/) for LazyVim and VS Code.
5. `lazymsbuild doctor` should show no failure.

From then on, build from the TUI or with `lazymsbuild build`, and the database follows.

## The TUI

`lazymsbuild` with no command starts it. The keys follow lazygit; every one can be changed in
`[keys]` of the configuration.

| Key | Action |
|---|---|
| `1`-`5`, `Tab`, `Shift+Tab`, `h`, `l` | Switch panes |
| `j`, `k`, `↑`, `↓`, `g`, `G`, `Ctrl+u`, `Ctrl+d` | Move, scroll |
| `[`, `]` | Switch tabs in a pane (Log / Errors) |
| `Enter`, `Space` | Select the target or the configuration |
| `/` | Filter the list |
| `?` | Show all keys |
| `x` | Menu with every action |
| `b`, `r`, `c` | Build, Rebuild, Clean (Clean asks first) |
| `s` | Sync (fill the database without building) |
| `m` | Toggle parallel builds (`-m`) |
| `e` | Open the selected error in `$EDITOR` (or `$VISUAL`, or the default application) |
| `Ctrl+c` | Cancel the build (asks first); quit when idle |
| `Esc` | Close a filter or a menu |
| `q` | Quit (asks first during a build) |

While a build runs you can move around, scroll the log and read the database status; another
build, a configuration switch, Clean and Sync wait until it ends.

## Commands

| Command | What it does |
|---|---|
| `lazymsbuild` | Start the TUI |
| `lazymsbuild init [--mode repo-root\|external] [--ignore exclude\|gitignore\|none]` | Write `.clangd` and keep generated files out of Git |
| `lazymsbuild sync [--profile C\|P]` | Fill the database without building |
| `lazymsbuild build [--target T] [--profile C\|P] [--rebuild\|--clean]` | Build without the TUI and update the database |
| `lazymsbuild check [--profile C\|P]` | Is the database complete and current? |
| `lazymsbuild doctor [--report]` | Is everything in place for clangd? (`--report` masks paths, for issues) |
| `lazymsbuild path [--db\|--state\|--config]` | Where things are (editors call this instead of computing paths) |
| `lazymsbuild completions <shell>` | Shell completion script (powershell, bash, zsh, fish, elvish) |

Common options: `--json` (machine-readable output), `-C <dir>` (as `git -C`), `--solution <file>`
(when a repository has several), `-q`, `-v`. Exit codes: `0` success, `1` failure (a failed build,
problems found), `2` wrong usage. Configurations are written `Configuration|Platform`; quote them
in a shell (`--profile "Release|x64"`). `--profile` also makes that configuration the active one.

## Where things go

In the repository (repo-root mode, the default):

| File | |
|---|---|
| `compile_commands.json` | The active configuration's database (`output.path` can move it, e.g. to `build/`) |
| `.clangd` | Written by `init` when missing; tells clangd to drop MSVC precompiled-header flags |
| `.cache/clangd/` | clangd's index (clangd creates it) |

`init` lists these in `.git/info/exclude`, which is local to your clone and changes no tracked
file. A `compile_commands.json` or `.clangd` that is already tracked is never written.

If nothing may be written into the repository, `lazymsbuild init --mode external` keeps the
database in the state directory instead; then only editors configured with
`--compile-commands-dir=$(lazymsbuild path --db)` use it, and clang-tidy does not find it.

Outside the repository:

| Path | |
|---|---|
| `%APPDATA%\lazymsbuild\config.toml` | Your settings (or `%XDG_CONFIG_HOME%\lazymsbuild`, or `%LAZYMSBUILD_CONFIG_DIR%`) |
| `.lazymsbuild.toml` at the repository root | Settings shared with the team (optional, committed) |
| `%LOCALAPPDATA%\lazymsbuild\<repo>-<hash>\` | State: one database per configuration, `status.json`, `errors.txt`, logs. Safe to delete |

## How the database stays complete

There are two ways to get compile commands out of MSBuild, and lazymsbuild uses both:

- **Design-time (`sync`)**: Microsoft's msbuild-extractor-sample evaluates every project without
  building. Complete, but only as current as its last run.
- **Build-time**: lazymsbuild's MSBuild logger records the command lines of every build. Exact
  and free, but a build only compiles what changed.

`sync` builds the database of a configuration; each build then updates the entries of the files
it compiled, replaces a project's entries when the project was rebuilt successfully, and drops
the entries of files that left their project. MSBuild hands cl.exe some settings outside the
command line (the *VC++ Directories*, the target architecture); lazymsbuild adds them to the
entries so clang tools see what the compiler saw. `docs/design.md` has the details and the
measurements behind them.

## Privacy

lazymsbuild makes no network connection, collects nothing and never updates itself. Its logs stay
in the state directory. When you report an issue, attach `lazymsbuild doctor --report` (paths
masked, no project names) and a minimal solution that reproduces it, never your company's logs or
solutions.

## Troubleshooting

- **`doctor` warns about hostfxr.dll**: the design-time extractor needs the .NET `hostfxr.dll` next
  to it when it cannot find Visual Studio's MSBuild; `scripts/install-extractor.ps1` puts it there.
- **"... is tracked by Git"**: the repository commits its own `compile_commands.json`; use
  `lazymsbuild init --mode external`, or stop tracking the file.
- **"... was not written by lazymsbuild"**: another tool wrote `compile_commands.json`. Pass
  `--force` (the TUI asks) to replace it.
- **"other databases"** in `check`: an older `build/compile_commands.json` (for example from a
  `:CompileCommands` setup) that clangd may read; delete it.
- **A project cannot be built on its own** in some configuration: the solution configuration does
  not build it, so MSBuild has no target for it.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The test solutions in `tests/fixtures/` are synthetic; please
keep it that way.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. Third-party notices: [THIRD_PARTY.md](THIRD_PARTY.md).
