<#
.SYNOPSIS
    Checks docs/editors/lazyvim.md's plugin file in a real LazyVim, without touching it.

.DESCRIPTION
    Copies your Neovim configuration and installed plugins (never links them: nothing here may
    write into the real ones) into a temporary directory, replaces any :CompileCommands of your own
    configuration with the plugin file taken from docs/editors/lazyvim.md, prepares a copy of the
    fixture solution, and runs tests\editors\lazyvim_check.lua in a headless Neovim.

    Needs: Neovim with LazyVim and its clangd extra installed, MSBuild, and lazymsbuild built
    (cargo build). The design-time extractor is optional.

.EXAMPLE
    pwsh -File tests\editors\lazyvim.ps1
#>
[CmdletBinding()]
param(
    [string]$NvimConfig = (Join-Path $env:LOCALAPPDATA 'nvim'),
    [string]$NvimData = (Join-Path $env:LOCALAPPDATA 'nvim-data'),
    [string]$Lazymsbuild = '',
    [switch]$Keep
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Lazymsbuild) { $Lazymsbuild = Join-Path $repoRoot 'target\debug\lazymsbuild.exe' }
if (-not (Test-Path $Lazymsbuild)) { throw "lazymsbuild not built: $Lazymsbuild (run cargo build)" }
$sandbox = Join-Path ([IO.Path]::GetTempPath()) ('lazymsbuild-lazyvim-' + [guid]::NewGuid().ToString('N').Substring(0, 8))

function Copy-Tree($from, $to) {
    robocopy $from $to /E /NFL /NDL /NJH /NJS /NP /XD .git | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy $from failed ($LASTEXITCODE)" }
}

try {
    # Neovim configuration and plugins, copied.
    Copy-Tree $NvimConfig (Join-Path $sandbox 'config\nvim')
    foreach ($dir in 'lazy', 'mason', 'site') {
        $source = Join-Path $NvimData $dir
        if (Test-Path $source) { Copy-Tree $source (Join-Path $sandbox "data\nvim-data\$dir") }
    }
    $plugins = Join-Path $sandbox 'config\nvim\lua\plugins'
    # A :CompileCommands of the configuration itself (dotfiles-windows has one) would hide ours.
    Get-ChildItem $plugins -Filter *.lua | Where-Object { (Get-Content $_.FullName -Raw) -match 'CompileCommands' } |
        ForEach-Object { Set-Content $_.FullName 'return {}' -Encoding utf8 }

    # The plugin file, exactly as the documentation has it.
    $doc = Get-Content (Join-Path $repoRoot 'docs\editors\lazyvim.md') -Raw
    $match = [regex]::Match($doc, '(?s)```lua\r?\n(.*?)```')
    if (-not $match.Success) { throw 'no lua block in docs/editors/lazyvim.md' }
    [IO.File]::WriteAllText((Join-Path $plugins 'lazymsbuild.lua'), $match.Groups[1].Value, (New-Object Text.UTF8Encoding($false)))

    # The fixture, filled once with a rebuild.
    $repo = Join-Path $sandbox 'repo'
    Copy-Tree (Join-Path $repoRoot 'tests\fixtures\basic') $repo
    $env:LAZYMSBUILD_STATE_DIR = Join-Path $sandbox 'state'
    $env:LAZYMSBUILD_CONFIG_DIR = Join-Path $sandbox 'lms-config'
    New-Item -ItemType Directory -Force $env:LAZYMSBUILD_CONFIG_DIR | Out-Null
    Push-Location $repo
    try {
        git init -q -b main; git add -A; git -c user.name=test -c user.email=test@example.invalid commit -qm fixture
        & $Lazymsbuild --solution Basic.sln init -q | Out-Null
        & $Lazymsbuild --solution Basic.sln build --rebuild -qq
        if ($LASTEXITCODE -ne 0) { throw "the fixture's rebuild failed ($LASTEXITCODE)" }
    } finally { Pop-Location }

    # Headless Neovim on the copies.
    $env:XDG_CONFIG_HOME = Join-Path $sandbox 'config'
    $env:XDG_DATA_HOME = Join-Path $sandbox 'data'
    $env:XDG_STATE_HOME = Join-Path $sandbox 'nvim-state'
    $env:XDG_CACHE_HOME = Join-Path $sandbox 'cache'
    $env:LAZYMSBUILD_TEST_REPO = $repo
    $env:PATH = (Split-Path -Parent $Lazymsbuild) + ';' + $env:PATH
    $check = Join-Path $PSScriptRoot 'lazyvim_check.lua'
    Push-Location $repo
    try {
        nvim --headless -c "lua dofile([[$check]])"
        $code = $LASTEXITCODE
    } finally { Pop-Location }
    if ($code -ne 0) { throw "the LazyVim check failed ($code)" }
} finally {
    if ($Keep) { Write-Host "kept $sandbox" } else { Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue }
}
