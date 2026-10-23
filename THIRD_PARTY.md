# Third-party software

## Used at run time, not distributed with lazymsbuild

| Software | License | How lazymsbuild uses it |
|---|---|---|
| [msbuild-extractor-sample](https://github.com/microsoft/msbuild-extractor-sample) (Microsoft) | MIT | Runs it for `sync` when it is installed. `scripts/install-extractor.ps1` downloads a pinned release from its GitHub Releases and checks its SHA256. |
| MSBuild and the Visual C++ build tools (Microsoft) | Visual Studio license terms | Runs the MSBuild of your Visual Studio or Build Tools installation. |
| [Microsoft.Build.Framework](https://www.nuget.org/packages/Microsoft.Build.Framework) | MIT | The logger is compiled against it; MSBuild provides it at run time, so it is not shipped. |

## Behaviour reproduced, no code copied

| Software | License | What |
|---|---|---|
| [Microsoft.VisualStudio.SolutionPersistence](https://github.com/microsoft/vs-solutionpersistence) | MIT | The rules that map `.slnx` solution configurations to project configurations. |
| [MSBuild](https://github.com/dotnet/msbuild) | MIT | How solution target names are formed and how their collisions are resolved. |
| [MsBuildCompileCommandsJson](https://github.com/0xabu/MsBuildCompileCommandsJson) (0xabu) | MIT | Reviewed as prior art for the build-time logger (the unquoted compiler path in `TaskCommandLineEventArgs`); see `docs/design.md`. |

## Compiled into lazymsbuild.exe

The Rust crates lazymsbuild depends on are listed in `Cargo.lock`. Their licenses (MIT, Apache-2.0
and compatible) are collected into `THIRD_PARTY_LICENSES.html` in each release archive.
