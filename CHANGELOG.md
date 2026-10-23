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
- Method A: `sync` through msbuild-extractor-sample, following `msbuild-extractor.json`.
- One database per configuration, with upsert, rebuild replacement, and garbage collection by the
  project's source list; published atomically to `compile_commands.json`.
- `init`, `sync`, `build`, `check`, `doctor`, `path` and `completions` commands.
