param(
    # Installer to run. Defaults to the one matching the version in Cargo.toml.
    [string]$Installer,
    # Skip the installer and only refresh the two EXEs from target/release.
    [switch]$BinariesOnly
)
# Developer/local deployment only. Keep any running GUI alive; a locked EXE can
# be renamed before replacement, leaving that process on its previous image.
#
# The version is read from Cargo.toml rather than defaulted here. A hard-coded
# default installed whatever release that filename happened to point at, and
# because the old script copied the EXEs *before* running the installer, a stale
# installer silently overwrote fresh binaries and the whole run looked clean.
# So: one deployment path per mode, and any disagreement with Cargo.toml is an
# error rather than a warning.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$version = [regex]::Match(
    (Get-Content $manifest -Raw),
    '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"'
).Groups[1].Value
if (-not $version) {throw "Cannot read [workspace.package] version from $manifest"}
Write-Output "Target version: $version"

$release = Join-Path $root 'target/release'
foreach ($name in @('feather.exe', 'feather-gui.exe')) {
    if (-not (Test-Path (Join-Path $release $name))) {
        throw "Missing $release\$name - build first: cargo build --release"
    }
}

$app = Join-Path $env:LOCALAPPDATA 'Programs/Feather'
# %USERPROFILE%\.feather\bin is an older manual-copy location. The installer has
# never written it and it is not on PATH, but -BinariesOnly has always refreshed
# it, so keep doing that rather than leaving a stale copy behind.
$legacy = Join-Path $env:USERPROFILE '.feather/bin'
# Only verify what this mode actually wrote; the installer owns {app} alone.
$dirs = if ($BinariesOnly) {@($legacy, $app)} else {@($app)}
$copyDirs = if ($BinariesOnly) {@($legacy, $app)} else {@()}

if ($BinariesOnly) {
    # No installer: target/release is the source of truth, so copy it directly.
    foreach ($dir in $copyDirs) {
        New-Item -ItemType Directory -Force $dir | Out-Null
        foreach ($name in @('feather.exe', 'feather-gui.exe')) {
            $source = (Resolve-Path (Join-Path $release $name)).Path
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
    Write-Output 'Binaries-only update: installer skipped.'
} else {
    # The installer is the single writer here; it embeds target/release as of the
    # last ISCC run, so copying first would only be overwritten by it.
    if (-not $Installer) {$Installer = Join-Path $root "dist/Feather-$version-Setup-x64.exe"}
    if (-not (Test-Path $Installer)) {
        throw "Installer not found: $Installer`nBuild it with: ISCC installer/Feather.iss"
    }
    $Installer = (Resolve-Path $Installer).Path
    Write-Output "Installer: $Installer"
    # Check *before* installing: a mismatch after the fact would mean the wrong
    # version had already been written to disk. Inno Setup stamps ProductVersion
    # from the .iss, and space-pads it to a fixed width, so trim before comparing.
    $packaged = "$((Get-Item $Installer).VersionInfo.ProductVersion)".Trim()
    if ($packaged -ne $version) {
        throw "Installer $Installer carries $packaged but Cargo.toml says $version - rebuild it with: ISCC installer/Feather.iss"
    }
    # The .iss copies target/release at compile time, so a binary newer than the
    # package means the installer predates the current build.
    $packagedAt = (Get-Item $Installer).LastWriteTimeUtc
    foreach ($name in @('feather.exe', 'feather-gui.exe')) {
        $builtAt = (Get-Item (Join-Path $release $name)).LastWriteTimeUtc
        if ($builtAt -gt $packagedAt) {
            throw "$name ($builtAt) is newer than the installer ($packagedAt) - rebuild it with: ISCC installer/Feather.iss"
        }
    }
    $p = Start-Process $Installer -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/NOCLOSEAPPLICATIONS' -PassThru -Wait
    if ($p.ExitCode -ne 0) {throw "Installer exit $($p.ExitCode)"}
    Write-Output "Installer exit: $($p.ExitCode)"
}

# The version the installed binary reports is the one invariant that catches a
# stale installer, a half-finished install and a shadowing shim at once.
$reported = (& (Join-Path $app 'feather.exe') --version 2>&1 | Out-String).Trim()
Write-Output "Installed: $reported"
if ($reported -ne "feather $version") {
    throw "Installed version '$reported' does not match Cargo.toml '$version' - the installer is stale; rebuild it with ISCC installer/Feather.iss"
}

foreach ($dir in $dirs) {
    foreach ($name in @('feather.exe', 'feather-gui.exe')) {
        $expected = (Get-FileHash (Join-Path $release $name)).Hash
        $actual = (Get-FileHash (Join-Path $dir $name)).Hash
        if ($actual -ne $expected) {
            throw "Hash mismatch: $dir/$name`n  installed $actual`n  target    $expected`nThe installer carries an older build - rebuild it with ISCC installer/Feather.iss"
        }
        Write-Output "$dir/$name hash verified"
    }
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathOk = $userPath -and ($userPath.Split(';') -contains $app)
Write-Output "User PATH contains installed app: $pathOk"
# An older per-user shim (e.g. %USERPROFILE%\.cargo\bin\feather.exe) earlier on
# PATH would keep running after an update. The installer puts {app} first; say
# so loudly when this machine still resolves elsewhere.
$shadow = $env:Path.Split(';') | Where-Object {
    $_ -and ($_.TrimEnd('\') -ne $app.TrimEnd('\')) -and (Test-Path (Join-Path $_ 'feather.exe'))
} | Select-Object -First 1
if ($shadow) {Write-Output "WARNING: another feather.exe is on PATH: $shadow"} else {Write-Output 'No shadowing feather.exe on PATH'}
# Absent when Feather has never been installed or was installed by a transient
# copy that refused to register; that is not a deployment failure.
if (Test-Path 'HKCU:/Software/Classes/Directory/Background/shell/FeatherGUI/command') {
    $explorerCommand = (Get-Item 'HKCU:/Software/Classes/Directory/Background/shell/FeatherGUI/command').GetValue('')
    Write-Output "Explorer command: $explorerCommand"
} else {
    Write-Output 'Explorer menu: not registered'
}
if (-not $BinariesOnly) {Get-FileHash $Installer | Format-List}
Get-Process feather-gui -ErrorAction SilentlyContinue | Select-Object Id,Path | Format-Table -AutoSize
