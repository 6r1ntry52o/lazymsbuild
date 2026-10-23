# lazymsbuild design notes

This file records the decisions behind lazymsbuild and the measurements they rest on.
User-facing behaviour is described in the README and in `docs/config.md`; this file is for
contributors.

## Phase 0: measurements (2026-10-23)

Environment: Visual Studio Build Tools 2022 17.14 (MSBuild 17.14.51, MSVC 14.44), Visual Studio
Community 2022 17.14 without the C++ workload, Windows SDK 10.0.26100, .NET SDK 9.0, clangd 23.1,
msbuild-extractor-sample 0.3.0. Test data: `tests/fixtures/basic` (solution folders nested two deep,
a project name with `.`, a name with spaces and parentheses, PCH, per-file defines, a source excluded
from Release, a project not built in Release|x64, a source path with spaces, a vendor SDK reached
only through `IncludePath`, a Win32 platform) and a generated 40-project, 2000-file solution.

### Name

`lazymsbuild` is free on crates.io (404), GitHub (no repository with the name) and winget (no
package).

### Extraction: design-time (A) versus build-time (B)

Same solution, same configuration (Debug|x64):

| | A: msbuild-extractor-sample | B: build-time logger |
|---|---|---|
| Files | 7 | 7 (the same 7) |
| `arguments[0]` | `...\bin\Hostx64\x64\cl.exe` | the compiler MSBuild ran (`HostX86` or `HostX64`, see below) |
| Only in A | `--target=x86_64-pc-windows-msvc`, `-ferror-limit=0`, and every `IncludePath` directory as `/I` | |
| Only in B | | `/ZI /JMC /nologo /diagnostics:column /sdl /Od /Gm- /RTC1 /GS /Zc:inline /Fo /Fd /FC /errorReport` |
| PCH flags (`/Yu /Yc /Fp`) | kept | kept |

The flags only B has are code generation, debug information and output switches; none changes how
a clang tool parses the code. What B lacks matters:

- **Include directories from `IncludePath` / `ExternalIncludePath`** (Project Properties > VC++
  Directories). `Microsoft.Cpp.Current.targets` hands them to cl.exe through the `INCLUDE` and
  `EXTERNAL_INCLUDE` environment variables (`SetEnv`), so they never appear on the command line.
  clangd finds the standard MSVC and Windows SDK headers by itself, but not a vendor SDK added there.
- **The target architecture.** Nothing on a cl.exe command line says x86 versus x64, so a clang tool
  reads a Win32 build as x64 (`_WIN64` defined, `sizeof(void*) == 8`).

Decision (U2): **both, in two stages.** `sync` builds the whole database with A (no build needed,
every file). After a build, B updates the entries of the files that were compiled, and B is the
authority for them because it is the exact command. To make B a superset of A, the logger records
`IncludePath` and `ExternalIncludePath` for every project evaluation, and lazymsbuild adds to each
B entry:

- `--target=<triple>` after the compiler, from the compiler's directory (`bin\Host*\x86\cl.exe` is
  `i686-pc-windows-msvc`, `x64` is `x86_64-pc-windows-msvc`, `arm64` is `aarch64-pc-windows-msvc`,
  `arm` is `thumbv7-pc-windows-msvc`; `/arm64EC` gives `arm64ec-pc-windows-msvc`), unless the command
  already sets a target (clang-cl);
- `/imsvc<dir>` for each `IncludePath` and `ExternalIncludePath` directory that exists, after the
  command's own options. clang-cl documents `/imsvc` as "add directory to system include search
  path, as if part of %INCLUDE%", which is exactly how cl.exe received them.

Entries of both methods also get `/Y-` after `/Yc` or `/Yu` (see clang-tidy below).

Option C (`.binlog` converted by a separate tool) is not an extraction method: there is no
`binlog2compilecommands` (GitHub search, 0 results). `build.keep_binlog` keeps a `.binlog` for
MSBuild Structured Log Viewer.

### The build-time logger

- `TaskCommandLineEventArgs` reaches a central logger (`-logger:`) with `-m`, with console verbosity
  `quiet` and `minimal`, from in-process and out-of-process nodes. `ProjectFile` is set on every event.
- The logged command line has the response file expanded (every flag is there). It starts with the
  compiler path **unquoted even when it contains spaces** (`C:\Program Files (x86)\...\CL.exe /c ...`);
  everything after the path follows the Windows (CRT) quoting rules (`/Fo"x64\Debug\\"`). Sources come
  last and are relative to the project directory, which is the compiler's working directory.
- One cl.exe call compiles several sources (a batch; with `/MP` every source of the project) and must
  be split into one entry per file. Sources with their own options get their own call.
- With `UseMultiToolTask=true` each source is its own call, and some are logged under the task name
  `TrackedExec` instead of `CL`. lazymsbuild therefore picks compiler calls by the executable name
  (`cl.exe`, `clang-cl.exe`), not by the task name.
- An incremental build with nothing to compile logs no compiler call at all.
- A logger that throws stops the build (`MSB4017`), so every handler in ours swallows its errors.
- Evaluated properties: with any custom logger attached, MSBuild already copies every property onto
  `ProjectStarted` (899 for a `.vcxproj`). Our logger calls `IEventSource4.IncludeEvaluationPropertiesAndItems()`
  so they travel once per evaluation instead, and records only `IncludePath` and `ExternalIncludePath`.
- Cost, 40 projects / 2000 sources, 16 cores, `-m`: Rebuild 35.6 s and 35.7 s with the logger,
  35.5 s and 35.8 s without; an up-to-date Build 1.5 s with, 1.4 s without.
- Existing loggers were reviewed and not reused: 0xabu/MsBuildCompileCommandsJson (MIT, last change
  2023-11) writes `command` strings with relative `file` values, keeps no project for an entry, only
  completes its JSON at shutdown (a cancelled build leaves nothing), selects `TaskName == "CL"` (misses
  `TrackedExec`) and recognizes only `.c`, `.cpp` and `.cxx`. Ours writes raw events and leaves all
  parsing to lazymsbuild, where it is unit-tested.

### Starting MSBuild

- **Quoting.** MSBuild.exe splits its own raw command line and does not understand `\"`. Any
  argument with a `"` inside breaks (`-logger:Class,"C:\x.dll";out` fails with MSB1021). An argument
  quoted as a whole works, with spaces and with non-ASCII characters
  (`"-logger:Class,C:\dir with space\テスト\x.dll;C:\out file.jsonl"`, `"-t:Libs\Native_Ext\Util _Legacy_:Rebuild"`).
  lazymsbuild never puts `"` inside an argument and lets the standard library quote whole arguments.
- **`-utf8output` does not exist** (`MSB1001: Unknown switch`; it is a csc.exe option). MSBuild
  writes its console output in the console's code page. lazymsbuild decodes each output line as
  UTF-8 when it is valid UTF-8 and otherwise in the code page Windows uses for console programs, and
  takes errors and warnings from the logger as structured events, so their text never goes through
  a code page.
- **Language.** `VSLANG=1033` does not change MSBuild.exe 17.14's messages; `DOTNET_CLI_UI_LANGUAGE=en-US`
  does. cl.exe follows `VSLANG` only when its English resources are installed (the machine above had
  only Japanese, `bin\Host*\*\1041`). Whatever the language, diagnostics keep the English words
  `error` / `warning` and their codes (`C2065`, `LNK2019`, `MSB8029`), which is what the parser keys on.
- **Bitness.** `MSBuild\Current\Bin\MSBuild.exe` is 32-bit and runs the x86-hosted compiler
  (`HostX86`); `Bin\amd64\MSBuild.exe` runs `HostX64`, as the 64-bit Visual Studio 2022 IDE does and
  as the design-time extractor does. For Visual Studio 2022 and later on 64-bit Windows lazymsbuild
  uses `Bin\amd64\MSBuild.exe` (`Bin\arm64\` on ARM64). The x86-hosted compiler is limited to 4 GB
  and can run out of heap on large translation units that build fine in the IDE.
- **Instance.** `vswhere -latest` can return an instance without the C++ workload (here VS Community
  had no `MSBuild\Microsoft\VC\v170\Microsoft.Cpp.Default.props`; MSBuild from it cannot build a
  `.vcxproj`). lazymsbuild only considers instances that have the VC targets.
- **Lingering processes.** `vctip.exe` (MSVC telemetry) and `mspdbsrv.exe` can outlive MSBuild.exe
  and hold inherited handles. Waiting for the process tree, or for end of output, hangs for minutes.
  lazymsbuild waits for MSBuild.exe itself, then closes the Job Object, which ends what is left.

### Target names (`-t:`)

Checked against the metaproject MSBuild writes with `MSBuildEmitSolution=1`, for `.sln` and `.slnx`
alike:

- the unique name is the solution-folder path joined with `\`, then the project name:
  `Libs\Native_Ext\Util _Legacy_` for project `Util (Legacy)` in folder `Libs\Native.Ext`;
- `. % $ @ ; ( ) '` become `_` in the whole name; spaces stay;
- when two cleaned names collide (`My.Lib` and `My_Lib`), the project whose name was changed by
  cleaning gets `_<project GUID without braces>` (`My_Lib_C0000000-0000-4000-8000-000000000001`);
- verbs are `<name>`, `<name>:Rebuild`, `<name>:Clean`;
- a project that is not built in the selected solution configuration (no `Build.0`) has no target
  at all, so building it by name fails (`MSB4057`). The TUI disables it.

### `.slnx` configurations

`.slnx` files do not list project configurations; MSBuild derives them with the rules of
Microsoft.VisualStudio.SolutionPersistence (MIT): built-in rules (same build type and platform as the
solution, built, not deployed), then the project type's rules (for `.vcxproj`: `Any CPU` -> `x64`,
`x86` -> `Win32`), then the project's own `<BuildType>`, `<Platform>`, `<Build>` and `<Deploy>`
elements. The last rule that applies wins. Verified against MSBuild's metaproject: the fixture's `.slnx`
maps exactly like its `.sln` (including x86 -> Win32 and `<Build Solution="Release|x64" Project="false" />`).

### clangd (23.1)

- `--check` passes on A's entries, on raw B entries and on extended B entries.
- `arguments[0]` as MSBuild logs it (`C:\...\HostX64\x64\CL.exe`, upper case) puts clangd in
  cl mode by itself: `--driver-mode=cl` is not needed.
- `/Yu`, `/Yc` and `/Fp` did not cause errors in clangd 23 even without a `.clangd`. `init` still writes
  the `Remove` rule, for older clangd versions and other clang tools.

### clang-tidy (19.1, from Visual Studio 2022's LLVM; 2026-10-24)

- Without `-p` it finds `compile_commands.json` at the repository root, as clangd does.
- It does not read `.clangd`, so precompiled headers broke it: with `/Yu`, "file 'x64\Debug\App.pch'
  is not a valid precompiled PCH file" (the driver turns `/Yu` + `/Fp` into `-include-pch` of
  MSVC's `.pch`); with `/Yc`, "expected exactly one compiler job" (building the PCH is a second job).
- Every entry with `/Yc` or `/Yu` therefore gets `/Y-` after it. cl.exe and clang-cl both take it as
  "ignore the other /Y options", so the entry stays a valid command for either, and clang tools parse
  the source with its header included normally, as clangd does on its own. The build's `/Y` options
  stay in the entry. Checked by `clang_tidy_reads_the_database_without_p`.
- clangd does not watch `compile_commands.json`, not even when the client offers
  `workspace/didChangeWatchedFiles` registration. It reloads it lazily: the next request that needs a
  compile command (an edit, opening a file) logs `Reloaded compilation database` and uses the new
  command. Open files therefore pick up a new database on their next edit, so restarting clangd after
  every database update is optional and off by default.

### The design-time extractor (A)

- It omits sources that are `ExcludedFromBuild` in the configuration, and includes projects that are
  not built in the configuration (no `Build.0`) as long as they have a configuration (`ActiveCfg`).
- Its output starts with a marker entry (`"file": ".msbuild-extractor-sample"`, `"command":
  "generated-by:..."`). lazymsbuild drops it on import.
- `msbuild-extractor.json` can set much more than the configuration (`msBuildProperties`,
  `msBuildEnv`, `vsInstance`, ...), so `sync` passes it with `--config`. It always passes `-o` and
  `-f standard`, and `-c`/`-a` only when the selected profile differs from the one the file declares.
  A file with `allConfigurations` produces entries of several configurations in one list that cannot
  be told apart, so `sync` then runs without the file and says so.
