# Native renderer backend smoke. Controls ONLY owned synthetic preview windows.
param(
    [string]$Executable='target/release/feather-gui.exe',
    [ValidateSet('cpu','wgpu','glow')][string]$Renderer='cpu'
)
$ErrorActionPreference='Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FeatherSmoke {
 [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out R r);
 [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int cx,int cy,uint flags);
 [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h,ref P p);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out P p);
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint dx,uint dy,uint data,UIntPtr extra);
 [StructLayout(LayoutKind.Sequential)] public struct P{public int X,Y;}
 [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback,IntPtr data);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
 delegate bool EnumProc(IntPtr h,IntPtr data);
 public static IntPtr PreviewWindow(uint pid) {
  IntPtr found=IntPtr.Zero; int largest=0;
  EnumWindows((h,data)=>{uint owner; R r;GetWindowThreadProcessId(h,out owner);
   if(owner==pid && IsWindowVisible(h) && GetClientRect(h,out r)) {
    int area=(r.Right-r.Left)*(r.Bottom-r.Top);if(area>largest){largest=area;found=h;}
   }
   return true;
  },IntPtr.Zero);
  return found;
 }
 [StructLayout(LayoutKind.Sequential)] public struct R{public int Left,Top,Right,Bottom;}
}
'@
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FeatherSmokeDpi {
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
}
'@
[FeatherSmokeDpi]::SetThreadDpiAwarenessContext([IntPtr](-4))|Out-Null
$originalPointer=New-Object FeatherSmoke+P
[FeatherSmoke]::GetCursorPos([ref]$originalPointer)|Out-Null
$env:FEATHER_UI_PREVIEW='1'
$env:FEATHER_UI_PREVIEW_EMPTY='1'
$env:FEATHER_GUI_RENDERER=$Renderer
$hitDir=Join-Path $env:TEMP ('feather-chrome-smoke-'+[guid]::NewGuid())
New-Item -ItemType Directory $hitDir|Out-Null
$env:FEATHER_UI_PREVIEW_RENDERER_HITS=$hitDir
Remove-Item Env:FEATHER_UI_CAPTURE -ErrorAction SilentlyContinue
$errorLog=Join-Path $hitDir 'stderr.txt'
$p=Start-Process (Resolve-Path $Executable).Path -RedirectStandardError $errorLog -PassThru
try {
 for($i=0;$i -lt 160;$i++){Start-Sleep -Milliseconds 50;$p.Refresh();$h=[FeatherSmoke]::PreviewWindow($p.Id);if($h -ne [IntPtr]::Zero){break}}
 if($h -eq [IntPtr]::Zero){throw 'No preview HWND'}
 [FeatherSmoke]::SetWindowPos($h,[IntPtr](-1),20,20,0,0,0x11)|Out-Null
 Start-Sleep -Milliseconds 400
 function Click($x,$y){
  $point=New-Object FeatherSmoke+P;$point.X=[int]$x;$point.Y=[int]$y
  [FeatherSmoke]::ClientToScreen($h,[ref]$point)|Out-Null
  [FeatherSmoke]::SetCursorPos($point.X-3,$point.Y+3)|Out-Null
  Start-Sleep -Milliseconds 80
  [FeatherSmoke]::SetCursorPos($point.X,$point.Y)|Out-Null
  Start-Sleep -Milliseconds 100
  [FeatherSmoke]::mouse_event(0x02,0,0,0,[UIntPtr]::Zero)
  Start-Sleep -Milliseconds 90
  [FeatherSmoke]::mouse_event(0x04,0,0,0,[UIntPtr]::Zero)
  Start-Sleep -Milliseconds 350
 }
 $r=New-Object FeatherSmoke+R
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 $scale=[FeatherSmoke]::GetDpiForWindow($h)/96.0
 Write-Host ("renderer={0} hwnd={1} client={2}x{3} dpiScale={4}" -f $Renderer,$h,$r.Right,$r.Bottom,$scale)
 Click ($r.Right-72*$scale) (30*$scale)
 Write-Host (Get-Content (Join-Path $hitDir 'Maximize.txt') -Raw)
 if(-not [FeatherSmoke]::IsZoomed($h)){throw 'Maximize click failed'}
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 Click ($r.Right-72*$scale) (30*$scale)
 # Restore may return Windows' old default placement. Reposition this same
 # preview so the actual pointer remains over the controls during the smoke.
 # Reposition the preview without activating it after restore.
 [FeatherSmoke]::SetCursorPos($originalPointer.X,$originalPointer.Y)|Out-Null
 Start-Sleep -Milliseconds 350
 [FeatherSmoke]::SetWindowPos($h,[IntPtr](-1),20,20,0,0,0x11)|Out-Null
 Start-Sleep -Milliseconds 200
 if([FeatherSmoke]::IsZoomed($h)){throw 'Restore click failed'}
 Write-Output 'Native maximize/restore: PASS'
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 Click ($r.Right-114*$scale) (30*$scale)
 Write-Host (Get-Content (Join-Path $hitDir 'Minimize.txt') -Raw)
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 Write-Host "After minimize native=$($r.Right)x$($r.Bottom) iconic=$([FeatherSmoke]::IsIconic($h)) zoomed=$([FeatherSmoke]::IsZoomed($h)) largestOwned=$([FeatherSmoke]::PreviewWindow($p.Id))"
 $p.Refresh();if($p.HasExited){Get-Content $errorLog;throw "Preview exited during minimize: $($p.ExitCode)"}
 if(-not [FeatherSmoke]::IsIconic($h)){throw 'Minimize click failed'}
 [FeatherSmoke]::SendMessage($h,0x112,[IntPtr]0xF120,[IntPtr]::Zero)|Out-Null
 Start-Sleep -Milliseconds 250
 if([FeatherSmoke]::IsIconic($h)){throw 'Restore after minimize failed'}
 Write-Output 'Native minimize/restore: PASS'
 # Resize + clipboard/paste / composer inputs exercise the same winit State.
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 Click ($r.Right*0.6) ($r.Bottom-110*$scale)
 foreach($c in 'INPUT SMOKE'.ToCharArray()){[FeatherSmoke]::SendMessage($h,0x102,[IntPtr][int]$c,[IntPtr]::Zero)|Out-Null}
 Start-Sleep -Milliseconds 200
 if($p.HasExited){throw 'Preview exited while typing'}
 Write-Output 'Native typed-input/no crash: PASS (manual/egui tests validate content)'
 # Synthetic messages after an OS minimize can retain stale pointer capture;
 # close through WM_CLOSE to test the host's native close-event path reliably.
 [FeatherSmoke]::SendMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero)|Out-Null
 if(-not $p.WaitForExit(5000)){throw 'Close click failed'}
 Write-Output 'Native close: PASS'
} finally {
 if(Test-Path $errorLog){$errorText=Get-Content $errorLog -Raw;if($errorText){Write-Host $errorText}}
 if(-not $p.HasExited){Stop-Process -Id $p.Id}
 $previewDir=Join-Path $env:TEMP ('feather-preview-'+$p.Id)
 if(Test-Path $previewDir){Remove-Item $previewDir -Recurse -Force}
 [FeatherSmoke]::SetCursorPos($originalPointer.X,$originalPointer.Y)|Out-Null
}
# A fresh-window close-button test (without preceding synthetic OS restore).
$p=Start-Process (Resolve-Path $Executable).Path -PassThru
try {
 for($i=0;$i -lt 160;$i++){Start-Sleep -Milliseconds 50;$p.Refresh();$h=[FeatherSmoke]::PreviewWindow($p.Id);if($h -ne [IntPtr]::Zero){break}}
 [FeatherSmoke]::SetWindowPos($h,[IntPtr](-1),20,20,0,0,0x11)|Out-Null
 Start-Sleep -Milliseconds 400
 $r=New-Object FeatherSmoke+R
 [FeatherSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 $scale=[FeatherSmoke]::GetDpiForWindow($h)/96.0
 Click ($r.Right-30*$scale) (30*$scale)
 if(-not $p.WaitForExit(5000)){throw 'Fresh header close button click failed'}
 Write-Output 'Fresh header close button: PASS'
} finally {
 if(-not $p.HasExited){Stop-Process -Id $p.Id}
 $previewDir=Join-Path $env:TEMP ('feather-preview-'+$p.Id)
 if(Test-Path $previewDir){Remove-Item $previewDir -Recurse -Force}
 [FeatherSmoke]::SetCursorPos($originalPointer.X,$originalPointer.Y)|Out-Null
}
