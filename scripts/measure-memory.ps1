# Offline RSS/private sampler for owned preview processes only. Launches
# FEATHER_UI_PREVIEW instances (synthetic data, temp home, no agent/config),
# samples working set + private bytes, then closes that same process.
param(
    [string]$Executable = 'target/release/feather-gui.exe',
    [int]$Seconds = 6,
    [string[]]$Renderers = @('auto','glow','wgpu'),
    [string]$Panel = 'empty'   # empty|normal|add
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Executable).Path
foreach ($r in $Renderers) {
    $env:FEATHER_UI_PREVIEW = '1'
    $env:FEATHER_UI_PREVIEW_EMPTY = if ($Panel -eq 'empty') {'1'} else {'0'}
    $env:FEATHER_UI_PREVIEW_PANEL = if ($Panel -eq 'add') {'add'} else {''}
    $env:FEATHER_UI_PREVIEW_SIZE = ''
    Remove-Item Env:FEATHER_UI_CAPTURE -EA SilentlyContinue
    if ($r -eq 'auto') { Remove-Item Env:FEATHER_GUI_RENDERER -EA SilentlyContinue }
    else { $env:FEATHER_GUI_RENDERER = $r }
    $p = Start-Process $exe -PassThru
    $peakWs = 0.0; $peakPriv = 0.0; $backend = ''
    try {
        for ($i = 0; $i -lt $Seconds * 2; $i++) {
            Start-Sleep -Milliseconds 500
            $p.Refresh()
            if ($p.HasExited) { break }
            $ws = $p.WorkingSet64 / 1MB
            $priv = $p.PrivateMemorySize64 / 1MB
            if ($ws -gt $peakWs) { $peakWs = $ws }
            if ($priv -gt $peakPriv) { $peakPriv = $priv }
        }
        $p.Refresh()
        $mods = @($p.Modules | Where-Object { $_.ModuleName -match 'vulkan|nvoglv64|opengl32|d3d12|ow-graphics' } | ForEach-Object { $_.ModuleName }) -join ','
        Write-Output ("renderer={0,-5} panel={1,-6} peakWS={2,7:N1}MB peakPriv={3,7:N1}MB  finalWS={4,7:N1}MB  gpuModules={5}" -f $r, $Panel, $peakWs, $peakPriv, ($p.WorkingSet64/1MB), $mods)
    } finally {
        if (-not $p.HasExited) { Stop-Process -Id $p.Id }
        $ph = Join-Path $env:TEMP ('feather-preview-' + $p.Id)
        if (Test-Path $ph) { Remove-Item $ph -Recurse -Force }
    }
}
Remove-Item Env:FEATHER_UI_PREVIEW,Env:FEATHER_UI_PREVIEW_EMPTY,Env:FEATHER_UI_PREVIEW_PANEL,Env:FEATHER_UI_PREVIEW_SIZE,Env:FEATHER_GUI_RENDERER -EA SilentlyContinue
