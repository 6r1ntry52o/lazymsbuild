# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The public formats
(`status.json`, the output of `lazymsbuild path`, the database and meta files, the build logger's
events) carry a `format_version` and follow the same rules.

## [Unreleased]

### Added

- `.sln` and `.slnx` parsing: C++ projects, nested solution folders, configuration mapping
  (including the `.slnx` rules of Microsoft.VisualStudio.SolutionPersistence), and MSBuild target
  names (`Libs\MyApp_Core`, collisions resolved like MSBuild).
- MSBuild discovery through vswhere, limited to installations with the C++ build tools, preferring
  the solution's Visual Studio version, and using the 64-bit MSBuild for Visual Studio 2022 and later.
- Builds in a Job Object (cancel ends the whole process tree), with `status.json`, `errors.txt` and
  per-build logs in the state directory, and `build.lock` against concurrent builds.
- The build logger (`LazyMsBuild.Logger.dll`) and method B: compile commands of every build,
  batches split per file, with the target architecture and `IncludePath` /
  `ExternalIncludePath` added.
- `/Y-` after precompiled-header options in every entry, so that clang-tidy (which does not read
  `.clangd`) works on sources that use a precompiled header.
- Method A: `sync` through msbuild-extractor-sample, following `msbuild-extractor.json`.
- One database per configuration, with upsert, rebuild replacement, and garbage collection by the
  project's source list; published atomically to `compile_commands.json`.
- Sources a target adds while the build runs (not listed in the project file) keep their entries
  and are not reported as orphans.
- The first configuration is Debug on this machine's platform when the solution has one.
- Paths are compared case-insensitively and with 8.3 short names (`C:\Users\RUNNER~1`) written in
  full, so a solution opened through `%TEMP%` matches what MSBuild reports.
- `init`, `sync`, `build`, `check`, `doctor`, `path` and `completions` commands, with `--json`,
  `-C`, `--solution`, `-q` and `-v`.
- `check`: coverage, stale projects, entries of deleted files, duplicates, whether
  `compile_commands.json` is the published database, other databases clangd would read, and
  whether the generated files are kept out of Git.
- `doctor`: the solution, MSBuild, the design-time extractor (and its `hostfxr.dll`), the build
  logger, clangd and its configuration, `.clangd`, Git and the database, and the tools alone when
  no solution can be chosen (`clangd --check` with `--check-locations=false`, at most 120 s per
  file); `--report` masks paths and leaves out project, file and
  configuration names, for bug reports.
- `--solution` is remembered: later commands and editors use the same solution.
- The TUI: targets as a tree of solution folders, configurations as a table of configuration and
  platform (without those no C++ project builds in), actions, log and errors, and
  the database status; lazygit keys, all configurable in `[keys]`; filter, menu and help;
  builds run in the background and can be cancelled; `e` opens an error in the editor at its
  line (`ui.edit_at_line`, `LAZYMSBUILD_EDITOR`); one pane at a time below 70 columns.
- Configuration in `config.toml` and the repository's `.lazymsbuild.toml`, with unknown keys
  reported as warnings, and `schema/config.schema.json` for editor completion.
- Editor guides: LazyVim (`docs/editors/lazyvim.md`: clangd, `:CompileCommands`, quickfix, a
  notification when a build ends, the TUI in a floating terminal) and VS Code
  (`docs/editors/vscode.md`).
- `docs/formats.md`: the public formats (`lazymsbuild path`, `status.json`, `errors.txt`, the
  database and meta files, the build logger's events).
- `scripts/install-extractor.ps1`: installs a pinned msbuild-extractor-sample, checked by its
  SHA256, and the `hostfxr.dll` it needs.
- Release archives for x64 and ARM64 with the build logger, `SHA256SUMS`, the third-party
  licenses, and cargo-binstall metadata.
- `README.ja.md`.
