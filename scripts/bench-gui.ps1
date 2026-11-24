# GUI frame-latency ruler.
#
# Runs the *real* GUI binary in its synthetic-preview mode (no agent, no config,
# temp home) and reports the frame cost the app measures for itself. This is the
# only honest way to measure rendering here: the app stamps `Instant::now()`
# around each frame, so the numbers include paint and present rather than a
# "how long did my code take" guess.
#
# Two report formats exist, and which one you get depends on the render backend:
#   software_window.rs -> frames / mean_frame_ms / mean_raster_ms / max_frame_ms
#                         / peak_vertices / peak_texture_texels
#   preview_timing.rs  -> frames / intervals / steady_mean_interval_ms
#                         / steady_max_interval_ms / elapsed_seconds
# The Windows default lands on the software path, so that is the primary one;
# the interval form is parsed too so a GPU renderer run still yields numbers.
#
# Usage:
#   scripts/bench-gui.ps1                       # default: 4s, auto renderer
#   scripts/bench-gui.ps1 -Seconds 8 -Json out.json
#   scripts/bench-gui.ps1 -Renderers auto,glow,wgpu
param(
    [string]$Executable = 'target/release/feather-gui.exe',
    [int]$Seconds = 4,
    [string[]]$Renderers = @('auto'),
    [string]$Json = ''
)
$ErrorActionPreference = 'Stop'

if (-not (Test-Path $Executable)) {
    throw "GUI binary not found: $Executable (build with: cargo build --release -p feather-gui)"
}
$exe = (Resolve-Path $Executable).Path
$results = @()

# One frame at 30 FPS. Matches the pacing target the preview mode asks for, so
# "over budget" means the app missed its own target rather than an arbitrary bar.
$budget = 33.333

foreach ($r in $Renderers) {
    $report = Join-Path $env:TEMP ("feather-frame-{0}-{1}.txt" -f $r, [guid]::NewGuid().ToString('N'))

    # Synthetic preview: fixed data set, no user config, self-closes after N seconds.
    $env:FEATHER_UI_PREVIEW = '1'
    $env:FEATHER_UI_PREVIEW_PACED = '1'
    $env:FEATHER_UI_PREVIEW_DURATION = "$Seconds"
    $env:FEATHER_GUI_FRAME_REPORT = $report
    if ($r -eq 'auto') { Remove-Item Env:FEATHER_GUI_RENDERER -ErrorAction SilentlyContinue }
    else { $env:FEATHER_GUI_RENDERER = $r }

    $p = $null
    try {
        $p = Start-Process $exe -PassThru
        # Wait for the app to finish (it self-closes on the duration), with a
        # hard cap so a hung run cannot block the ruler.
        $deadline = (Get-Date).AddSeconds($Seconds + 20)
        while (-not $p.HasExited -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 200
        }
        if (-not $p.HasExited) { throw "GUI did not exit within $($Seconds + 20)s" }
    } finally {
        if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    }

    if (-not (Test-Path $report)) {
        Write-Warning "renderer=$r produced no frame report (expected $report)"
        $results += [pscustomobject]@{
            renderer = $r; frames = 0; mean_ms = $null; raster_ms = $null
            max_ms = $null; budget_ms = $budget; verdict = 'no-data'
        }
        continue
    }

    # The app rewrites this file on its close path; the last write can land just
    # after the process handle reports exit, so re-read until the fields parse.
    $values = @{}
    for ($attempt = 0; $attempt -lt 15; $attempt++) {
        $values = @{}
        foreach ($line in (Get-Content $report -ErrorAction SilentlyContinue)) {
            if ($line -match '^\s*([a-z_0-9]+)=(.+)$') { $values[$Matches[1]] = $Matches[2].Trim() }
        }
        if ($values.ContainsKey('mean_frame_ms') -or $values.ContainsKey('steady_mean_interval_ms')) {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    Remove-Item $report -Force -ErrorAction SilentlyContinue

    $frames = if ($values.ContainsKey('frames')) { [int]$values['frames'] } else { 0 }

    if ($values.ContainsKey('mean_frame_ms')) {
        # Software raster path: mean_frame_ms is the whole CPU frame, of which
        # mean_raster_ms is the blit/draw share.
        $mean = [double]$values['mean_frame_ms']
        $raster = [double]$values['mean_raster_ms']
        $max = [double]$values['max_frame_ms']
        $source = 'software'
    }
    elseif ($values.ContainsKey('steady_mean_interval_ms')) {
        # GPU path: this is the wall-clock interval between presented frames.
        $mean = [double]$values['steady_mean_interval_ms']
        $raster = $null
        $max = [double]$values['steady_max_interval_ms']
        $source = 'interval'
    }
    else {
        Write-Warning "renderer=$r frame report has neither mean_frame_ms nor steady_mean_interval_ms"
        $results += [pscustomobject]@{
            renderer = $r; frames = $frames; mean_ms = $null; raster_ms = $null
            max_ms = $null; budget_ms = $budget; verdict = 'no-data'
        }
        continue
    }

    # A paced run's interval is the pacing target, not the work performed, so
    # judge the software frame cost against the budget and just report intervals.
    $verdict = if ($source -eq 'interval') { 'paced' }
               elseif ($mean -le $budget) { 'ok' }
               else { 'over-budget' }

    $results += [pscustomobject]@{
        renderer  = $r
        source    = $source
        frames    = $frames
        mean_ms   = [math]::Round($mean, 3)
        raster_ms = if ($null -ne $raster) { [math]::Round($raster, 3) } else { $null }
        max_ms    = [math]::Round($max, 3)
        budget_ms = $budget
        verdict   = $verdict
    }
}

Remove-Item Env:FEATHER_UI_PREVIEW,Env:FEATHER_UI_PREVIEW_PACED,Env:FEATHER_UI_PREVIEW_DURATION,Env:FEATHER_GUI_FRAME_REPORT,Env:FEATHER_GUI_RENDERER -ErrorAction SilentlyContinue

foreach ($row in $results) {
    # A "no report" row has null numbers; format those as a dash rather than
    # letting `-f` throw on a null with a numeric format specifier.
    $meanText = if ($null -ne $row.mean_ms) { '{0:N2}' -f $row.mean_ms } else { '-' }
    $rasterText = if ($null -ne $row.raster_ms) { '{0:N2}' -f $row.raster_ms } else { '-' }
    $maxText = if ($null -ne $row.max_ms) { '{0:N2}' -f $row.max_ms } else { '-' }
    Write-Output ("renderer={0,-5} src={1,-8} frames={2,5}  mean={3}ms  raster={4}ms  max={5}ms  budget={6:N2}ms  {7}" -f `
        $row.renderer, $row.source, $row.frames, $meanText, $rasterText, $maxText, $row.budget_ms, $row.verdict)
}

if ($Json) {
    $payload = [pscustomobject]@{
        seconds   = $Seconds
        budget_ms = $budget
        cases     = $results
        timestamp = (Get-Date).ToUniversalTime().ToString('o')
    }
    $dir = Split-Path -Parent $Json
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $payload | ConvertTo-Json -Depth 4 | Set-Content -Path $Json -Encoding utf8
    Write-Output "wrote $Json"
}
