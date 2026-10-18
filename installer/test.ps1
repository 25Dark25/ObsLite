# Installs, upgrades and uninstalls the MSI for the current user, checking files, shortcuts and
# clean-up at each step. Run by CI; also safe to run by hand (it uninstalls what it installed).
#   installer/test.ps1 -Msi obs-lite.msi -UpgradeMsi obs-lite-newer.msi
param(
    [Parameter(Mandatory)] [string] $Msi,
    # The same installer with a higher version, to check that an upgrade keeps the choices.
    [Parameter(Mandatory)] [string] $UpgradeMsi
)
$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\obs-lite'
$startMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Obs Lite.lnk'
$desktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Obs Lite.lnk'
$exe = Join-Path $installDir 'obs-lite.exe'

function Msiexec([string[]] $arguments) {
    $log = Join-Path $env:TEMP 'obs-lite-msi.log'
    $p = Start-Process msiexec.exe -ArgumentList ($arguments + @('/qn', '/l*v', "`"$log`"")) -Wait -PassThru
    if ($p.ExitCode -ne 0) {
        Get-Content $log -Tail 40
        throw "msiexec $arguments failed with exit code $($p.ExitCode)"
    }
}

function Check([bool] $ok, [string] $what) {
    if (-not $ok) { throw "FAILED: $what" }
    "ok: $what"
}

function ShortcutTarget([string] $path) {
    (New-Object -ComObject WScript.Shell).CreateShortcut($path).TargetPath
}

# What Settings › Apps lists: per-user MSI products are registered under the user's SID.
function Installed {
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    Get-ChildItem "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\UserData\$sid\Products" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-Item (Join-Path $_.PSPath 'InstallProperties') -ErrorAction SilentlyContinue } |
        Where-Object { $_.GetValue('DisplayName') -eq 'Obs Lite' }
}

function CheckRemoved {
    Check (-not (Test-Path $installDir)) 'install folder removed'
    Check (-not (Test-Path $startMenu)) 'Start menu shortcut removed'
    Check (-not (Test-Path $desktop)) 'no desktop shortcut left'
    Check (-not (Installed)) 'no entry left in Installed apps'
    Check (-not (Test-Path 'HKCU:\Software\obs-lite\Installer')) 'installer registry key removed'
}

Check (-not (Installed)) 'Obs Lite is not installed before the test'

# 1. Default install: Start menu only.
Msiexec @('/i', "`"$Msi`"")
foreach ($file in 'obs-lite.exe', 'LICENSE.txt', 'THIRD-PARTY-LICENSES.html') {
    Check (Test-Path (Join-Path $installDir $file)) "$file installed"
}
Check ((ShortcutTarget $startMenu) -eq $exe) 'Start menu shortcut "Obs Lite" points to obs-lite.exe'
Check (-not (Test-Path $desktop)) 'no desktop shortcut by default'
$entry = Installed
Check ($null -ne $entry) 'listed in Installed apps as "Obs Lite"'
Check ($entry.GetValue('InstallLocation') -eq "$installDir\") 'Installed apps entry has the install folder'
$icon = Get-ChildItem 'HKCU:\Software\Microsoft\Installer\Products' |
    Where-Object { $_.GetValue('ProductName') -eq 'Obs Lite' } | ForEach-Object { $_.GetValue('ProductIcon') }
Check ([bool] $icon -and (Test-Path $icon)) 'Installed apps entry has the obs-lite icon'
Msiexec @('/x', "`"$Msi`"")
CheckRemoved

# 2. Desktop shortcut chosen; an upgrade keeps it without being asked again.
Msiexec @('/i', "`"$Msi`"", 'DESKTOPSHORTCUT=1')
Check ((ShortcutTarget $desktop) -eq $exe) 'desktop shortcut "Obs Lite" points to obs-lite.exe'
$before = (Installed).GetValue('DisplayVersion')
Msiexec @('/i', "`"$UpgradeMsi`"")
$after = Installed
Check (@($after).Count -eq 1 -and $after.GetValue('DisplayVersion') -ne $before) "upgraded in place ($before -> $($after.GetValue('DisplayVersion')))"
Check ((ShortcutTarget $desktop) -eq $exe) 'desktop shortcut kept by the upgrade'
Check ((ShortcutTarget $startMenu) -eq $exe) 'Start menu shortcut kept by the upgrade'
Msiexec @('/x', "`"$UpgradeMsi`"")
CheckRemoved
