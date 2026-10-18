# Builds the MSI from a release build (`cargo build --release` first). Needs WiX 6.0.2 and its UI
# extension on the PATH:
#   dotnet tool install --global wix --version 6.0.2
#   wix extension add --global WixToolset.UI.wixext/6.0.2
#   installer/build.ps1 -BinDir <folder with obs-lite.exe, LICENSE, THIRD-PARTY-LICENSES.html> -Out obs-lite.msi
param(
    [Parameter(Mandatory)] [string] $BinDir,
    [Parameter(Mandatory)] [string] $Out,
    # Defaults to the version in Cargo.toml.
    [string] $Version
)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot
if (-not $Version) {
    $Version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
}
# build.rs writes the icon it embeds in the executable next to its other outputs.
$icon = Get-ChildItem (Join-Path $root 'target\release\build\obs-lite-*\out\icon.ico') |
    Sort-Object LastWriteTime | Select-Object -Last 1
if (-not $icon) { throw 'icon.ico not found: run cargo build --release first' }

wix build (Join-Path $PSScriptRoot 'obs-lite.wxs') -arch x64 -ext WixToolset.UI.wixext `
    -d "Version=$Version" -d "BinDir=$BinDir" -d "Icon=$($icon.FullName)" -o $Out
if ($LASTEXITCODE) { throw "wix build failed with exit code $LASTEXITCODE" }
