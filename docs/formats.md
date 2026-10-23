# Public formats

Editors and scripts may read these. Each has a `format_version`. Adding a field is backwards
compatible and keeps the version; renaming, removing or changing the meaning of a field raises it,
and is listed in `CHANGELOG.md`. Readers should ignore fields they do not know.

Paths are absolute, times are RFC 3339 with the local offset (`2026-10-23T12:00:00.000+09:00`),
files are UTF-8 without a BOM.

## `lazymsbuild path`

Where things are, so that an editor never computes lazymsbuild's directories itself.

| Command | Prints |
|---|---|
| `lazymsbuild path --db` | The directory of the published `compile_commands.json` (clangd's `--compile-commands-dir`) |
| `lazymsbuild path --state` | The state directory (`status.json`, `errors.txt`, `logs\`) |
| `lazymsbuild path --config` | The user configuration directory |
| `lazymsbuild --json path` | All of them: |

```json
{
  "format_version": 1,
  "solution": "C:\\src\\repo\\MySolution.sln",
  "root": "C:\\src\\repo",
  "mode": "repo-root",
  "profile": "Debug|x64",
  "db": "C:\\src\\repo",
  "db_file": "C:\\src\\repo\\compile_commands.json",
  "state": "C:\\Users\\you\\AppData\\Local\\lazymsbuild\\repo-1a2b3c4d",
  "status": "C:\\Users\\you\\AppData\\Local\\lazymsbuild\\repo-1a2b3c4d\\status.json",
  "errors": "C:\\Users\\you\\AppData\\Local\\lazymsbuild\\repo-1a2b3c4d\\errors.txt",
  "config": "C:\\Users\\you\\AppData\\Roaming\\lazymsbuild"
}
```

All of them take `-C <dir>` to ask about another directory's project. With several solutions in a
repository, the one used last is chosen.

## `status.json`

In the state directory. Written when a build or sync starts, every second while it runs, at every
state change, and when it ends. It is always replaced as a whole (a new file renamed over the old
one), so watch the **directory** for changes to `status.json`, not the file itself.

```json
{
  "format_version": 1,
  "state": "building",
  "profile": "Debug|x64",
  "target": "Libs\\MyApp_Core",
  "verb": "Build",
  "solution": "C:\\src\\repo\\MySolution.sln",
  "started_at": "2026-10-23T12:00:00.000+09:00",
  "finished_at": null,
  "errors": 0,
  "warnings": 3,
  "pid": 12345,
  "log": "C:\\Users\\you\\AppData\\Local\\lazymsbuild\\repo-1a2b3c4d\\logs\\20261023-120000.log",
  "db": {
    "path": "C:\\src\\repo\\compile_commands.json",
    "updated_at": "2026-10-23T11:58:02.114+09:00",
    "entries": 342,
    "delta": 2,
    "coverage": [342, 350],
    "stale_projects": []
  }
}
```

| Field | |
|---|---|
| `state` | `idle`, `syncing`, `building` (running), `succeeded`, `failed`, `cancelled` (ended) |
| `profile` | The configuration, `Configuration\|Platform` |
| `target` | The project's solution target name; `null` for the whole solution |
| `verb` | `Build`, `Rebuild`, `Clean` or `Sync` |
| `errors`, `warnings` | Counts so far, final once the run has ended |
| `pid` | The lazymsbuild process that runs it |
| `db` | The database after the last change: `delta` is the change in entries, `coverage` is [sources with an entry, sources compiled], `stale_projects` are the projects changed since their entries were captured. Missing before the first database exists |

A run has ended when `state` is not `syncing` or `building`. A new run has a new `started_at`;
compare it too, since a short run can start and end between two looks.

## `errors.txt`

In the state directory: the last build's errors, then its warnings, one per line, in the MSVC
format that Neovim's default `errorformat` and VS Code's `$msCompile` read:

```text
C:\src\repo\src\Core\core.cpp(19,23): error C2065: 'undeclared_name': undeclared identifier
LINK : error LNK1104: cannot open file 'x.lib'
```

A fatal error is written as `error` (its code, `C1xxx`, says it is fatal); a message without a
place starts with its tool (`LINK`, `MSBUILD`).

## The databases

`compile_commands.json` follows the [JSON Compilation Database](https://clang.llvm.org/docs/JSONCompilationDatabase.html)
specification: `directory`, `file` (absolute), `arguments` (or `command` with
`output.format = "command"`), and `output` when known. Entries are sorted by file, so the same
database always gives the same bytes.

Each configuration's master copy is `profiles\<solution>-<hash>\<Config>_<Platform>.json` in the
state directory, next to its `.meta.json`:

```json
{
  "format_version": 1,
  "solution": "C:\\src\\repo\\MySolution.sln",
  "profile": "Debug|x64",
  "updated_at": "2026-10-23T12:01:05.000+09:00",
  "entries": {
    "c:\\src\\repo\\src\\core\\core.cpp": {
      "project": "C:\\src\\repo\\src\\Core\\MyApp.Core.vcxproj",
      "method": "build",
      "updated_at": "2026-10-23T12:01:05.000+09:00"
    }
  },
  "projects": {
    "c:\\src\\repo\\src\\core\\myapp.core.vcxproj": {
      "path": "C:\\src\\repo\\src\\Core\\MyApp.Core.vcxproj",
      "captured_at": "2026-10-23T12:00:00.000+09:00",
      "method": "build"
    }
  },
  "last_build": {
    "verb": "Build", "target": null, "result": "succeeded",
    "started_at": "2026-10-23T12:00:00.000+09:00", "finished_at": "2026-10-23T12:01:05.000+09:00",
    "errors": 0, "warnings": 3
  }
}
```

Keys are lower-case normalized paths. `method` is `design-time` (sync) or `build`. A meta file
of a newer format version is not misread: lazymsbuild starts its bookkeeping over (the entries
stay) until the next sync or build.

## The build logger's events

`LazyMsBuild.Logger.dll` writes one JSON object per line to the file lazymsbuild gives it; the
first line is `{"t":"hello","v":1,"msbuild":"17.14.51.32402"}`, and lazymsbuild refuses another
`v`. The lines are listed in `logger/CompileCommandLogger.cs`. This format is internal to a
lazymsbuild release (the DLL ships with the exe), so it may change between releases.
