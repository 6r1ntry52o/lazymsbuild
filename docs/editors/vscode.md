# VS Code

In repo-root mode (the default) VS Code needs only clangd: it finds `compile_commands.json` and
`.clangd` at the repository root by itself.

## 1. clangd

1. Install the **clangd** extension (`llvm-vs-code-extensions.vscode-clangd`). It downloads clangd
   when there is none.
2. When it asks, let it turn off IntelliSense of Microsoft's C/C++ extension; two language servers
   on the same files disagree.
3. In the repository: `lazymsbuild init`, then `lazymsbuild sync` (or a rebuild from the TUI).
4. Open a `.cpp` file. *Go to Definition* and the diagnostics use the solution's include paths and
   defines.

clangd reads a changed `compile_commands.json` again the next time it needs a compile command (the
next edit of an open file); *clangd: Restart language server* makes it read it at once.

## 2. Building from VS Code

`lazymsbuild build -q` prints only errors and warnings, in the MSVC format VS Code's `$msCompile`
problem matcher reads, so they appear in *Problems* with links to their place. Put this in
`.vscode/tasks.json`:

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "lazymsbuild: build",
      "type": "process",
      "command": "lazymsbuild",
      "args": ["build", "-q"],
      "problemMatcher": "$msCompile",
      "group": { "kind": "build", "isDefault": true }
    },
    {
      "label": "lazymsbuild: rebuild",
      "type": "process",
      "command": "lazymsbuild",
      "args": ["build", "--rebuild", "-q"],
      "problemMatcher": "$msCompile",
      "group": "build"
    },
    {
      "label": "lazymsbuild: sync",
      "type": "process",
      "command": "lazymsbuild",
      "args": ["sync"],
      "problemMatcher": []
    },
    {
      "label": "lazymsbuild",
      "type": "process",
      "command": "lazymsbuild",
      "problemMatcher": [],
      "presentation": { "panel": "dedicated", "focus": true }
    }
  ]
}
```

`Ctrl+Shift+B` runs the build; the last task starts the TUI in a terminal panel. The errors of
the last build, however it was started, are also in `errors.txt` in the state directory
(`lazymsbuild path --state`).

## External mode

With `lazymsbuild init --mode external`, tell clangd where the database is, in
`.vscode/settings.json` (the path is what `lazymsbuild path --db` prints):

```json
{ "clangd.arguments": ["--compile-commands-dir=C:\\Users\\you\\AppData\\Local\\lazymsbuild\\repo-1a2b3c4d\\active"] }
```

and add the block `lazymsbuild init --mode external` prints to `%LOCALAPPDATA%\clangd\config.yaml`.
