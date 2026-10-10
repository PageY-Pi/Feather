; Build with Inno Setup 6: ISCC installer\Feather.iss
; Strict allowlist: no home directory, databases, models, auth or sessions.
#define AppVersion "1.0.0"
[Setup]
AppId={{BA63A0FA-5EDB-4C50-923B-56FEA51D6168}
AppName=Feather
AppVersion={#AppVersion}
AppPublisher=Feather
DefaultDirName={localappdata}\Programs\Feather
DefaultGroupName=Feather
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=Feather-{#AppVersion}-Setup-x64
SetupIconFile=..\crates\feather-gui\assets\feather.ico
UninstallDisplayIcon={app}\feather-gui.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
DisableProgramGroupPage=yes
[Files]
Source: "..\target\release\feather.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\feather-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
[Icons]
Name: "{group}\Feather"; Filename: "{app}\feather-gui.exe"; WorkingDir: "{userdocs}"
Name: "{userdesktop}\Feather"; Filename: "{app}\feather-gui.exe"; WorkingDir: "{userdocs}"
[Registry]
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\FeatherGUI"; ValueType: string; ValueName: ""; ValueData: "Open Feather here"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\FeatherGUI"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\feather-gui.exe"
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\FeatherGUI\command"; ValueType: string; ValueName: ""; ValueData: """{app}\feather-gui.exe"" ""%V"""
Root: HKCU; Subkey: "Software\Classes\Directory\shell\FeatherGUI"; ValueType: string; ValueName: ""; ValueData: "Open Feather here"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\FeatherGUI"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\feather-gui.exe"
Root: HKCU; Subkey: "Software\Classes\Directory\shell\FeatherGUI\command"; ValueType: string; ValueName: ""; ValueData: """{app}\feather-gui.exe"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\DesktopBackground\Shell\FeatherGUI"; ValueType: string; ValueName: ""; ValueData: "Open Feather here"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\DesktopBackground\Shell\FeatherGUI"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\feather-gui.exe"
Root: HKCU; Subkey: "Software\Classes\DesktopBackground\Shell\FeatherGUI\command"; ValueType: string; ValueName: ""; ValueData: """{app}\feather-gui.exe"" ""%V"""
[Run]
Filename: "{app}\feather-gui.exe"; Description: "Open Feather and configure your API with + Add API"; WorkingDir: "{userdocs}"; Flags: nowait postinstall skipifsilent
[Code]
function SplitString(Value, Separator: String): TArrayOfString;
var P, N: Integer;
begin
  SetArrayLength(Result, 0);
  repeat
    P := Pos(Separator, Value); N := GetArrayLength(Result);
    SetArrayLength(Result, N+1);
    if P = 0 then begin Result[N] := Value; Value := ''; end
    else begin Result[N] := Copy(Value, 1, P-1); Delete(Value, 1, P); end;
  until P = 0;
end;
function ContainsPath(Value, Entry: String): Boolean;
var Parts: TArrayOfString; I: Integer;
begin
  Result := False;
  Parts := SplitString(Value, ';');
  for I := 0 to GetArrayLength(Parts)-1 do
    if CompareText(RemoveBackslashUnlessRoot(Trim(Parts[I])), RemoveBackslashUnlessRoot(Entry)) = 0 then begin Result := True; Exit; end;
end;
procedure CurStepChanged(CurStep: TSetupStep);
var Value, AppDir: String;
begin
  if CurStep = ssPostInstall then begin
    AppDir := ExpandConstant('{app}');
    RegQueryStringValue(HKCU, 'Environment', 'Path', Value);
    if not ContainsPath(Value, AppDir) then begin
      if (Value <> '') and (Value[Length(Value)] <> ';') then Value := Value + ';';
      if RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Value + AppDir) then
        RegWriteStringValue(HKCU, 'Software\Feather\Installer', 'AddedPath', AppDir);
    end;
  end;
end;
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var Value, Added, NewValue: String; Parts: TArrayOfString; I: Integer;
begin
  if CurUninstallStep = usUninstall then begin
    if RegQueryStringValue(HKCU, 'Software\Feather\Installer', 'AddedPath', Added) then begin
      if CompareText(Added, ExpandConstant('{app}')) = 0 then begin
        RegQueryStringValue(HKCU, 'Environment', 'Path', Value);
        Parts := SplitString(Value, ';'); NewValue := '';
        for I := 0 to GetArrayLength(Parts)-1 do
          if CompareText(RemoveBackslashUnlessRoot(Trim(Parts[I])), RemoveBackslashUnlessRoot(Added)) <> 0 then begin
            if NewValue <> '' then NewValue := NewValue + ';';
            NewValue := NewValue + Parts[I];
          end;
        RegWriteExpandStringValue(HKCU, 'Environment', 'Path', NewValue);
        RegDeleteValue(HKCU, 'Software\Feather\Installer', 'AddedPath');
      end;
    end;
  end;
end;
