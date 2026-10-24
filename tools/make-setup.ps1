# Builds what a GitHub release carries, into dist\: nowbar-setup.exe (both exes inside),
# nowbar.exe and nowbar-settings.exe (what the updater downloads) and SHA256SUMS.txt.
#
#   powershell -ExecutionPolicy Bypass -File tools\make-setup.ps1 [-Jobs 2] [-KeepBuild]
#
# The C runtime is linked statically: the exes open on a Windows without the Visual C++
# redistributable. The dist profile is release with thin LTO and stripped symbols. Its build
# (a few GB, used only for a release) is deleted at the end unless -KeepBuild.
param([int]$Jobs = 2, [switch]$KeepBuild)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
try {
    $meta = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $version = ($meta.packages | Where-Object name -eq 'nowbar').version
    $built = Join-Path $meta.target_directory 'dist'
    $dist = Join-Path $repo 'dist'
    $payload = Join-Path $dist 'payload'

    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build -j $Jobs --profile dist --bin nowbar --bin nowbar-settings
    if ($LASTEXITCODE) { throw 'the exes did not build' }
    New-Item -ItemType Directory -Force $payload | Out-Null
    foreach ($exe in 'nowbar.exe', 'nowbar-settings.exe') { Copy-Item (Join-Path $built $exe) $payload -Force }

    $env:NOWBAR_SETUP_PAYLOAD = $payload
    $env:NOWBAR_VERSION = $version
    cargo build -j $Jobs --profile dist -p nowbar-setup
    if ($LASTEXITCODE) { throw 'the setup did not build' }
    Copy-Item (Join-Path $built 'nowbar-setup.exe') $dist -Force
    foreach ($exe in 'nowbar.exe', 'nowbar-settings.exe') { Copy-Item (Join-Path $payload $exe) $dist -Force }

    # The updater's format: "<sha256>  <name>" per line.
    $sums = foreach ($exe in 'nowbar-setup.exe', 'nowbar.exe', 'nowbar-settings.exe') {
        '{0}  {1}' -f (Get-FileHash (Join-Path $dist $exe) -Algorithm SHA256).Hash.ToLower(), $exe
    }
    Set-Content (Join-Path $dist 'SHA256SUMS.txt') $sums -Encoding ascii

    # No exe may need the Visual C++ runtime DLLs.
    foreach ($exe in 'nowbar-setup.exe', 'nowbar.exe', 'nowbar-settings.exe') {
        $bytes = [System.IO.File]::ReadAllBytes((Join-Path $dist $exe))
        $text = [System.Text.Encoding]::ASCII.GetString($bytes)
        if ($text -match '(?i)vcruntime140|msvcp140') { throw "$exe still imports the Visual C++ runtime" }
        '{0,-22} {1,6:N1} MB' -f $exe, ($bytes.Length / 1MB)
    }
    "v$version -> $dist"
    Remove-Item $payload -Recurse -Force
    if (-not $KeepBuild) {
        Remove-Item $built -Recurse -Force -ErrorAction SilentlyContinue
        'the dist build was deleted (-KeepBuild keeps it)'
    }
}
finally {
    Pop-Location
}
