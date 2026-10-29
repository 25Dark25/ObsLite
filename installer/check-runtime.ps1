# Fails if obs-lite.exe needs the Visual C++ runtime (VCRUNTIME140.dll and the like): a fresh
# Windows does not have it, and obs-lite would not start there.
param([string]$Exe = "target/release/obs-lite.exe")
$ErrorActionPreference = "Stop"
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Select-Object -First 1
$deps = & $dumpbin.FullName /dependents $Exe | Where-Object { $_ -match '\.dll' } | ForEach-Object { $_.Trim() }
$deps
$runtime = $deps | Where-Object { $_ -match '^(vcruntime|msvcp|api-ms-win-crt-)' }
if ($runtime) { throw "$Exe needs the Visual C++ runtime: $($runtime -join ', ')" }
