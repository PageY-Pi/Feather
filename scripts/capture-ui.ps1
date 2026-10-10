param(
    [string]$Executable = 'target/release/feather-gui.exe',
    [string]$OutputDir = 'target/ui-review/final'
)
# Only synthetic preview processes. Hover goes through egui's raw input hook;
# no real pointer movement, secret/config reads or user's app termination.
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Executable).Path
New-Item -ItemType Directory -Force $OutputDir | Out-Null
$out = (Resolve-Path $OutputDir).Path
$variants = @('normal', 'compact', 'add', 'compact-add', 'empty', 'close-hover', 'maximized', 'commands', 'renderer', 'compact-renderer')
foreach ($variant in $variants) {
    $env:FEATHER_UI_PREVIEW = '1'
    $env:FEATHER_UI_PREVIEW_SCROLL = 'top'
    $env:FEATHER_UI_PREVIEW_SIZE = if ($variant -like 'compact*') {'compact'} elseif ($variant -eq 'maximized') {'maximized'} else {''}
    $env:FEATHER_UI_PREVIEW_PANEL = if ($variant -like '*add') {'add'} elseif ($variant -eq 'commands') {'commands'} elseif ($variant -like '*renderer') {'renderer'} else {''}
    $env:FEATHER_UI_PREVIEW_EMPTY = if ($variant -eq 'empty') {'1'} else {''}
    $env:FEATHER_UI_PREVIEW_HOVER = if ($variant -eq 'close-hover') {'close'} else {''}
    $env:FEATHER_UI_CAPTURE = Join-Path $out ($variant + '.png')
    $p = Start-Process $exe -PassThru
    try {
        if (-not $p.WaitForExit(25000)) {throw 'Preview screenshot timed out'}
        if ($p.ExitCode -ne 0) {throw "Preview exit: $($p.ExitCode)"}
        Get-Item $env:FEATHER_UI_CAPTURE | Select-Object Name,Length
    } finally {
        if (-not $p.HasExited) {Stop-Process -Id $p.Id} # Only our preview process.
        $previewHome = Join-Path $env:TEMP ('feather-preview-' + $p.Id)
        if (Test-Path $previewHome) {Remove-Item $previewHome -Recurse -Force}
    }
}
foreach ($name in @('FEATHER_UI_PREVIEW','FEATHER_UI_PREVIEW_SCROLL','FEATHER_UI_PREVIEW_SIZE','FEATHER_UI_PREVIEW_PANEL','FEATHER_UI_PREVIEW_EMPTY','FEATHER_UI_PREVIEW_HOVER','FEATHER_UI_CAPTURE')) {
    Remove-Item ('Env:' + $name) -ErrorAction SilentlyContinue
}
