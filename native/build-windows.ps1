#!/usr/bin/env pwsh
# Build the Unterm native terminal and install it as a Unity Windows plugin DLL.
#
# Unity loads native plugins on Windows from a .dll. A Rust cdylib already builds
# `unterm.dll`, so we just copy it into the package's Plugins/Windows/x86_64.
[CmdletBinding()]
param(
    # 'release' or 'debug'. Named -Configuration to avoid PowerShell's automatic
    # $PROFILE variable.
    [ValidateSet('release', 'debug')]
    [string]$Configuration = 'release',

    # Default to the MSVC ABI that matches the Unity Editor. Pass
    # x86_64-pc-windows-gnu on a machine that only has the GNU toolchain.
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot

# Keep host paths out of debug information and derive timestamps from the source
# commit so repeated builds of the same revision have stable inputs.
if ([string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
    $env:SOURCE_DATE_EPOCH = (git -C (Join-Path $PSScriptRoot '..') log -1 --pretty=%ct)
}
$nativeRoot = (Resolve-Path -LiteralPath $PSScriptRoot).Path
$env:RUSTFLAGS = (($env:RUSTFLAGS + " --remap-path-prefix=$nativeRoot=.").Trim())

$cargoFlags = @()
$targetDir = 'debug'
if ($Configuration -eq 'release') {
    $cargoFlags += '--release'
    $targetDir = 'release'
}

# Idempotent (already-installed is fine). Pipe only stdout to Out-Null; do NOT
# redirect stderr — under $ErrorActionPreference='Stop', redirecting a native
# command's stderr turns its progress text into a terminating error.
rustup target add $Target | Out-Null

Write-Host "==> building unterm ($Configuration, $Target)"
cargo build -p unterm --locked --lib --bin unterm-debugger @cargoFlags --target $Target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

$pluginDir = Join-Path $PSScriptRoot '..\Packages\dev.tnayuki.unterm\Editor\Plugins\Windows\x86_64'
$libDest = Join-Path $pluginDir 'unterm.dll'
$debuggerDest = Join-Path $pluginDir 'unterm-debugger.exe'
New-Item -ItemType Directory -Force -Path $pluginDir | Out-Null

$lib = Join-Path $PSScriptRoot "target\$Target\$targetDir\unterm.dll"
if (-not (Test-Path -LiteralPath $lib)) { throw "built dll not found: $lib" }
$debugger = Join-Path $PSScriptRoot "target\$Target\$targetDir\unterm-debugger.exe"
if (-not (Test-Path -LiteralPath $debugger)) { throw "built debugger not found: $debugger" }

Write-Host "==> copy -> $libDest"
Copy-Item -LiteralPath $lib -Destination $libDest -Force
Write-Host "==> copy -> $debuggerDest"
Copy-Item -LiteralPath $debugger -Destination $debuggerDest -Force

Write-Host "==> done: $libDest"
Write-Host "==> done: $debuggerDest"
