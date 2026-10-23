# Contributing

Thank you for helping. Bug reports, fixes, editor integrations and documentation are all welcome.

## Never share private code or logs

lazymsbuild is used on company machines. In issues, pull requests, tests and examples, use only
synthetic solutions. Do not paste build logs, solution or project files, project names, paths or
`compile_commands.json` from a real codebase. `lazymsbuild doctor --report` masks paths and leaves
out project names, and a small solution that reproduces the problem is the best bug report.

## Setting up

- Rust 1.89 or later (`rustup`)
- Visual Studio 2019 or later, or the Build Tools, with *Desktop development with C++*
- The .NET SDK (6 or later), for the MSBuild logger in `logger/`
- Optional: msbuild-extractor-sample (`scripts/install-extractor.ps1`) and clangd, for `sync`
  and `doctor`

```powershell
dotnet build logger -c Release   # LazyMsBuild.Logger.dll; debug builds of lazymsbuild find it here
cargo build
cargo test --all                 # unit tests, and integration tests when MSBuild is installed
```

The integration tests copy the solutions in `tests/fixtures/` to a temporary directory and build
them with MSBuild. Without MSBuild they skip themselves; set `LAZYMSBUILD_REQUIRE_MSBUILD=1` to make
that a failure instead, as CI does.

## Changes

- `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` must pass.
- Add tests with the change. Behaviour that depends on MSBuild belongs in an integration test
  against a fixture; please add a fixture (synthetic) rather than mocking MSBuild.
- A decision that is not obvious from the code goes into `docs/design.md`, with what it was based on.
- `status.json`, the `path` output, the database and meta files and the logger's events are public
  formats with a `format_version`: additions are fine, anything else is a new version.
- Add a line to `CHANGELOG.md` under *Unreleased*.
- User-visible text is in English.

## Releasing

Push a tag `v<version>` after updating `CHANGELOG.md` and the version in `Cargo.toml`; the release
workflow builds the archives and their `SHA256SUMS`.
