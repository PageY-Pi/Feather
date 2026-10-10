# Process-local thread instruction sampling. No monitor/driver changes, no
# termination, no user text/config/session data. PSS copies register state only.
param([int]$ProcessId,[int]$Samples=24,[int]$IntervalMs=100)
$ErrorActionPreference='Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FeatherThreadSample {
 [StructLayout(LayoutKind.Sequential)] public struct FT {public uint Low,High;}
 [StructLayout(LayoutKind.Sequential)] public struct Entry {
  public uint ExitStatus;public IntPtr Teb;public uint Pid,Tid;public UIntPtr Affinity;
  public int Priority,BasePriority;public IntPtr LastArg;public ushort Syscall;
  public FT Create,Exit,Kernel,User;public IntPtr Start;public FT Capture;
  public uint Flags;public ushort Suspend,ContextSize;public IntPtr Context;
 }
 [DllImport("kernel32.dll")] public static extern uint PssCaptureSnapshot(IntPtr p,uint flags,uint context,out IntPtr snapshot);
 [DllImport("kernel32.dll")] public static extern uint PssFreeSnapshot(IntPtr p,IntPtr snapshot);
 [DllImport("kernel32.dll")] public static extern uint PssWalkMarkerCreate(IntPtr allocator,out IntPtr marker);
 [DllImport("kernel32.dll")] public static extern uint PssWalkMarkerFree(IntPtr marker);
 [DllImport("kernel32.dll")] public static extern uint PssWalkSnapshot(IntPtr snapshot,int kind,IntPtr marker,out Entry entry,uint size);
 [DllImport("dbghelp.dll",CharSet=CharSet.Unicode)] public static extern bool SymInitializeW(IntPtr p,string path,bool invade);
 [DllImport("dbghelp.dll")] public static extern bool SymCleanup(IntPtr p);
 [DllImport("dbghelp.dll")] public static extern uint SymSetOptions(uint options);
 [DllImport("dbghelp.dll")] public static extern bool StackWalk64(uint machine,IntPtr p,IntPtr thread,IntPtr frame,IntPtr context,IntPtr read,IntPtr table,IntPtr module,IntPtr translate);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern IntPtr LoadLibraryW(string path);
 [DllImport("kernel32.dll",CharSet=CharSet.Ansi)] static extern IntPtr GetProcAddress(IntPtr module,string name);
 public static ulong[] Stack(IntPtr process,IntPtr context,int bytes) {
  byte[] data=new byte[bytes];Marshal.Copy(context,data,0,bytes);
  IntPtr copy=Marshal.AllocHGlobal(bytes),frame=Marshal.AllocHGlobal(1024);
  try {
   Marshal.Copy(data,0,copy,bytes);Marshal.Copy(new byte[1024],0,frame,1024);
   Marshal.WriteInt64(frame,0,Marshal.ReadInt64(context,248));Marshal.WriteInt32(frame,12,3);
   Marshal.WriteInt64(frame,32,Marshal.ReadInt64(context,160));Marshal.WriteInt32(frame,44,3);
   Marshal.WriteInt64(frame,48,Marshal.ReadInt64(context,152));Marshal.WriteInt32(frame,60,3);
   var lib=LoadLibraryW("dbghelp.dll");var table=GetProcAddress(lib,"SymFunctionTableAccess64");var mod=GetProcAddress(lib,"SymGetModuleBase64");
   var ips=new System.Collections.Generic.List<ulong>();ips.Add((ulong)Marshal.ReadInt64(context,248));
   for(int i=0;i<20;i++) {if(!StackWalk64(0x8664,process,IntPtr.Zero,frame,copy,IntPtr.Zero,table,mod,IntPtr.Zero))break;
    ulong ip=(ulong)Marshal.ReadInt64(frame,0);if(ip==0)break;ips.Add(ip);
   }return ips.ToArray();
  }finally{Marshal.FreeHGlobal(copy);Marshal.FreeHGlobal(frame);}
 }
}
'@
$p=Get-Process -Id $ProcessId
$modules=@($p.Modules|ForEach-Object{[pscustomobject]@{Name=$_.ModuleName;Base=[uint64]$_.BaseAddress.ToInt64();Size=[uint64]$_.ModuleMemorySize}})
[FeatherThreadSample]::SymSetOptions([Convert]::ToUInt32('A0000294',16))|Out-Null
if(-not [FeatherThreadSample]::SymInitializeW($p.Handle,'', $true)){throw 'Local symbol initialization failed'}
$before=@{};foreach($t in $p.Threads){$before[$t.Id]=$t.TotalProcessorTime.TotalSeconds}
Start-Sleep -Milliseconds 500;$p.Refresh()
$hot=@($p.Threads|Sort-Object @{Expression={$_.TotalProcessorTime.TotalSeconds-$before[$_.Id]};Descending=$true}|Select-Object -First 3 -ExpandProperty Id)
"Sampling PID=$ProcessId hot threads=$($hot -join ',') PSS entry bytes=$([Runtime.InteropServices.Marshal]::SizeOf([type][FeatherThreadSample+Entry]))"
$result=@()
for($i=0;$i -lt $Samples;$i++) {
 $snap=[IntPtr]::Zero;$marker=[IntPtr]::Zero
 try {
  $err=[FeatherThreadSample]::PssCaptureSnapshot($p.Handle,0x180,0x100001,[ref]$snap)
  if($err -ne 0){throw "PssCaptureSnapshot error $err"}
  $err=[FeatherThreadSample]::PssWalkMarkerCreate([IntPtr]::Zero,[ref]$marker)
  if($err -ne 0){throw "PssWalkMarkerCreate error $err"}
  while($true) {
   $entry=New-Object FeatherThreadSample+Entry
   $err=[FeatherThreadSample]::PssWalkSnapshot($snap,3,$marker,[ref]$entry,[Runtime.InteropServices.Marshal]::SizeOf([type][FeatherThreadSample+Entry]))
   if($err -eq 259){break};if($err -ne 0){throw "PssWalkSnapshot error $err"}
   if($entry.Tid -in $hot -and $entry.Context -ne [IntPtr]::Zero -and $entry.ContextSize -gt 256) {
    $ip=[uint64][Runtime.InteropServices.Marshal]::ReadInt64($entry.Context,248)
    $module=$modules|Where-Object{$ip -ge $_.Base -and $ip -lt ($_.Base+$_.Size)}|Select-Object -First 1
    $result+=@([pscustomobject]@{Thread=$entry.Tid;Module=$module.Name;Offset=if($module){'{0:X}' -f ($ip-$module.Base)}else{'{0:X}' -f $ip};Syscall=$entry.Syscall})
    if($entry.Tid -eq $hot[0] -and $i -lt 8) {
     $stack=@([FeatherThreadSample]::Stack($p.Handle,$entry.Context,$entry.ContextSize)|ForEach-Object {
      $address=$_;$m=$modules|Where-Object{$address -ge $_.Base -and $address -lt ($_.Base+$_.Size)}|Select-Object -First 1
      if($m){'{0}+0x{1:X}' -f $m.Name,($address-$m.Base)}else{'0x{0:X}' -f $address}
     });"Stack $i thread $($entry.Tid): $($stack -join ' <- ')"
    }
   }
  }
 } finally {
  if($marker -ne [IntPtr]::Zero){[FeatherThreadSample]::PssWalkMarkerFree($marker)|Out-Null}
  if($snap -ne [IntPtr]::Zero){[FeatherThreadSample]::PssFreeSnapshot($p.Handle,$snap)|Out-Null}
 }
 Start-Sleep -Milliseconds $IntervalMs
}
$result|Group-Object Thread,Module,Offset|Sort-Object Count -Descending|Select-Object Count,Name|Format-Table -AutoSize
[FeatherThreadSample]::SymCleanup($p.Handle)|Out-Null
