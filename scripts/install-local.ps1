param(
    [string]$Installer = 'dist/Feather-1.2.0-Setup-x64.exe',
    [switch]$BinariesOnly
)
# Developer/local deployment only. Keep any running GUI alive; a locked EXE can
# be renamed before replacement, leaving that process on its previous image.
$ErrorActionPreference = 'Stop'
$dirs = @((Join-Path $env:USERPROFILE '.feather/bin'), (Join-Path $env:LOCALAPPDATA 'Programs/Feather'))
foreach ($dir in $dirs) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    foreach ($name in @('feather.exe', 'feather-gui.exe')) {
        $source = (Resolve-Path (Join-Path 'target/release' $name)).Path
        $target = Join-Path $dir $name
        try {
            Copy-Item $source $target -Force
            Write-Output "Updated $target"
        } catch {
            if (-not (Test-Path $target)) {throw}
            $backup = Join-Path $dir ($name.Replace('.exe', '.previous-' + (Get-Date -Format 'yyyyMMddHHmmss') + '.exe'))
            Move-Item $target $backup
            try {Copy-Item $source $target -Force} catch {Move-Item $backup $target -Force; throw}
            Write-Output "Updated $target; running executable preserved at $backup"
        }
    }
}
if (-not $BinariesOnly) {
    $p = Start-Process (Resolve-Path $Installer).Path -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/NOCLOSEAPPLICATIONS' -PassThru -Wait
    if ($p.ExitCode -ne 0) {throw "Installer exit $($p.ExitCode)"}
    Write-Output "Installer exit: $($p.ExitCode)"
} else {
    Write-Output 'Binaries-only update: installer skipped.'
}
$app = Join-Path $env:LOCALAPPDATA 'Programs/Feather'
& (Join-Path $app 'feather.exe') --version
foreach ($dir in $dirs) {
    foreach ($name in @('feather.exe', 'feather-gui.exe')) {
        $expected = (Get-FileHash (Join-Path 'target/release' $name)).Hash
        $actual = (Get-FileHash (Join-Path $dir $name)).Hash
        if ($actual -ne $expected) {throw "Hash mismatch: $dir/$name"}
        Write-Output "$dir/$name hash verified"
    }
}
$pathOk = [Environment]::GetEnvironmentVariable('Path','User').Split(';') -contains $app
Write-Output "User PATH contains installed app: $pathOk"
# An older per-user shim (e.g. %USERPROFILE%\.cargo\bin\feather.exe) earlier on
# PATH would keep running after an update. The installer puts {app} first; say
# so loudly when this machine still resolves elsewhere.
$shadow = $env:Path.Split(';') | Where-Object {
    $_ -and ($_.TrimEnd('\') -ne $app.TrimEnd('\')) -and (Test-Path (Join-Path $_ 'feather.exe'))
} | Select-Object -First 1
if ($shadow) {Write-Output "WARNING: another feather.exe is on PATH: $shadow"} else {Write-Output 'No shadowing feather.exe on PATH'}
$explorerCommand = (Get-Item 'HKCU:/Software/Classes/Directory/Background/shell/FeatherGUI/command').GetValue('')
Write-Output "Explorer command: $explorerCommand"
if (-not $BinariesOnly) {Get-FileHash $Installer | Format-List}
Get-Process feather-gui -ErrorAction SilentlyContinue | Select-Object Id,Path | Format-Table -AutoSize
