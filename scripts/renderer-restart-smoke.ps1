# Native renderer selection/restart regression. Only controls its synthetic
# previews; never enumerates or closes the user's running Feather windows.
param([string]$Executable='target/release/feather-gui.exe')
$ErrorActionPreference='Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FeatherRestartSmoke {
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int cx,int cy,uint flags);
 [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out R r);
 [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h,ref P p);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out P p);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint dx,uint dy,uint data,UIntPtr extra);
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
 [StructLayout(LayoutKind.Sequential)] public struct P{public int X,Y;}
 [StructLayout(LayoutKind.Sequential)] public struct R{public int Left,Top,Right,Bottom;}
}
'@
[FeatherRestartSmoke]::SetThreadDpiAwarenessContext([IntPtr](-4))|Out-Null
$env:FEATHER_UI_PREVIEW='1';$env:FEATHER_UI_PREVIEW_EMPTY='1';$env:FEATHER_GUI_RENDERER='cpu'
$env:FEATHER_UI_PREVIEW_BLOCKING_SHUTDOWN='1'
Remove-Item Env:FEATHER_UI_CAPTURE,Env:FEATHER_UI_PREVIEW_HOVER -ErrorAction SilentlyContinue
$reportDir=Join-Path $env:TEMP ('feather-render-smoke-'+[guid]::NewGuid())
New-Item -ItemType Directory $reportDir|Out-Null
$originalPointer=New-Object FeatherRestartSmoke+P
[FeatherRestartSmoke]::GetCursorPos([ref]$originalPointer)|Out-Null
$env:FEATHER_UI_PREVIEW_RENDERER_HITS=$reportDir
$env:FEATHER_GUI_GRAPHICS_REPORT=Join-Path $reportDir 'renderer.txt'
$owned=@();$homes=@()
function GetWindow($p){
 for($i=0;$i -lt 160;$i++){Start-Sleep -Milliseconds 50;$p.Refresh();if($p.HasExited){throw 'Preview exited before window'};$h=[FeatherRestartSmoke]::PreviewWindow($p.Id);if($h -ne [IntPtr]::Zero){Start-Sleep -Milliseconds 350;return $h}}
 throw 'No HWND'
}
function Click($h,$x,$y){
 $point=New-Object FeatherRestartSmoke+P;$point.X=[int]$x;$point.Y=[int]$y
 [FeatherRestartSmoke]::ClientToScreen($h,[ref]$point)|Out-Null
 [FeatherRestartSmoke]::SetCursorPos($point.X-3,$point.Y+3)|Out-Null
 Start-Sleep -Milliseconds 80
 [FeatherRestartSmoke]::SetCursorPos($point.X,$point.Y)|Out-Null
 Start-Sleep -Milliseconds 100
 [FeatherRestartSmoke]::mouse_event(0x02,0,0,0,[UIntPtr]::Zero)
 Start-Sleep -Milliseconds 90
 [FeatherRestartSmoke]::mouse_event(0x04,0,0,0,[UIntPtr]::Zero)
 Start-Sleep -Milliseconds 350
}
function SwitchRenderer($p,$wanted){
 $h=GetWindow $p
 # Keep the owned preview visible on this monitor; don't resize/maximize an
 # unrelated user window. Some OS default placements put the footer off-screen.
 [FeatherRestartSmoke]::SetWindowPos($h,[IntPtr](-1),20,20,0,0,0x11)|Out-Null
 Start-Sleep -Milliseconds 350
 $r=New-Object FeatherRestartSmoke+R;[FeatherRestartSmoke]::GetClientRect($h,[ref]$r)|Out-Null
 $s=[FeatherRestartSmoke]::GetDpiForWindow($h)/96.0
 $hit=Get-Content (Join-Path $reportDir 'selector.txt') -Raw
 $xy=$hit.Split(' ')
 Write-Host ("Selector hit {0}, native {1}x{2}, scale {3}" -f $hit,$r.Right,$r.Bottom,$s)
 Click $h ([float]::Parse($xy[0],[cultureinfo]::InvariantCulture)*$s) ([float]::Parse($xy[1],[cultureinfo]::InvariantCulture)*$s)
 Start-Sleep -Milliseconds 250
 # Render label opens a centered renderer chooser instead of an inline combo.
 for($i=0;$i -lt 40 -and -not(Test-Path (Join-Path $reportDir ($wanted+'.txt')));$i++){Start-Sleep -Milliseconds 50}
 if(-not(Test-Path (Join-Path $reportDir ($wanted+'.txt')))){throw 'Renderer chooser did not open after native label click'}
 $hit=Get-Content (Join-Path $reportDir ($wanted+'.txt')) -Raw
 $xy=$hit.Split(' ')
 Click $h ([float]::Parse($xy[0],[cultureinfo]::InvariantCulture)*$s) ([float]::Parse($xy[1],[cultureinfo]::InvariantCulture)*$s)
 if(-not $p.WaitForExit(7000)){throw "Old preview did not exit after selecting $wanted"}
 for($i=0;$i -lt 160;$i++){
  Start-Sleep -Milliseconds 50
  if(Test-Path $env:FEATHER_GUI_GRAPHICS_REPORT){
   $text=Get-Content $env:FEATHER_GUI_GRAPHICS_REPORT -Raw
   if($text -match "renderer=$wanted`n" -and $text -match 'pid=(\d+)'){
    $childId=[int]$Matches[1]
    if($childId -ne $p.Id){return Get-Process -Id $childId}
   }
  }
 }
 throw "No $wanted child report after restart"
}
try{
 $p=Start-Process (Resolve-Path $Executable).Path -PassThru;$owned+=@($p);$homes+=@($p.Id)
 $p=SwitchRenderer $p 'wgpu';$owned+=@($p);$homes+=@($p.Id)
 if((Get-Content $env:FEATHER_GUI_GRAPHICS_REPORT -Raw) -notmatch 'backend=Dx12'){throw 'GPU child did not use native Direct3D 12'}
 Write-Output 'Native selector CPU -> Direct3D 12 automatic restart: PASS'
 $p=SwitchRenderer $p 'cpu';$owned+=@($p);$homes+=@($p.Id)
 Write-Output 'Native selector Direct3D 12 -> CPU automatic restart: PASS'
 $h=GetWindow $p
 # Native close while minimized has no redraw event; must still exit.
 [FeatherRestartSmoke]::SendMessage($h,0x112,[IntPtr]0xF020,[IntPtr]::Zero)|Out-Null
 Start-Sleep -Milliseconds 250
 [FeatherRestartSmoke]::SendMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero)|Out-Null
 if(-not $p.WaitForExit(5000)){throw 'Minimized close blocked'}
 Write-Output 'Minimized close with 60-second blocking job: PASS'
}finally{
 # Reports contain only owned preview child PIDs; cover children from a failed
 # selector test as well so no synthetic window is left behind.
 if(Test-Path $env:FEATHER_GUI_GRAPHICS_REPORT){$text=Get-Content $env:FEATHER_GUI_GRAPHICS_REPORT -Raw;if($text -match 'pid=(\d+)'){$id=[int]$Matches[1];$owned+=@(Get-Process -Id $id -ErrorAction SilentlyContinue);$homes+=@($id)}}
 foreach($p in $owned){if($p -and -not $p.HasExited){Stop-Process -Id $p.Id}}
 foreach($id in $homes){$dir=Join-Path $env:TEMP ('feather-preview-'+$id);if(Test-Path $dir){Remove-Item $dir -Recurse -Force}}
 Remove-Item $reportDir -Recurse -Force
 [FeatherRestartSmoke]::SetCursorPos($originalPointer.X,$originalPointer.Y)|Out-Null
}
