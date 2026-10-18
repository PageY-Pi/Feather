; Build with Inno Setup 6: ISCC installer\Feather.iss
; Strict allowlist: no home directory, databases, models, auth or sessions.
#define AppVersion "1.1.0"
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
Source: "..\third-party-notices.md"; DestDir: "{app}"; Flags: ignoreversion
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
{ ---------------------------------------------------------------------------
  Host prerequisites.

  Feather runs model-issued commands through Git Bash and drives file
  checkpoints with git. Neither is bundled: shipping them would add ~60 MB and
  pull a GPLv2 redistribution obligation into this installer. A bare Windows
  machine can therefore finish setup with an unusable Bash tool, so the wizard
  detects Git for Windows and offers to install it.

  Invariant: this step must never fail the install. Any network, UAC or
  installer failure degrades to a clear message plus the download page.
--------------------------------------------------------------------------- }
#include "prereq_git.iss"

const
  GitInstallerName = 'Git-64-bit.exe';
  GitSilentParams =
    '/VERYSILENT /NORESTART /SP- /NOCANCEL /CLOSEAPPLICATIONS /SUPPRESSMSGBOXES';

var
  PrereqPage: TWizardPage;
  PrereqInfo: TNewStaticText;
  PrereqAuto, PrereqBrowser, PrereqSkip: TNewRadioButton;
  DownloadPage: TDownloadWizardPage;
  GitReady: Boolean;


function InstallGitForWindows(): Boolean;
var
  Url, Installer: String;
  ResultCode: Integer;
begin
  Result := False;
  Url := ResolveGitInstallerUrl();
  if Url = '' then
    Exit;
  Installer := ExpandConstant('{tmp}\') + GitInstallerName;
  DownloadPage.Clear;
  DownloadPage.Add(Url, GitInstallerName, '');
  Log('prereq: downloading Git for Windows from ' + Url + ' (' + GitSource + ')');
  DownloadPage.Show;
  try
    try
      DownloadPage.Download;
    except
      Log('prereq: download failed from ' + Url);
      Exit;
    end;
  finally
    DownloadPage.Hide;
  end;
  if not FileExists(Installer) then
    Exit;
  if not Exec(Installer, GitSilentParams, '', SW_SHOW, ewWaitUntilTerminated,
    ResultCode) then
    Exit;
  GitReady := GitDetected();
  Result := GitReady;
end;

procedure InitializeWizard();
begin
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing),
    SetupMessage(msgPreparingDesc), nil);
  DownloadPage.ShowBaseNameInsteadOfUrl := True;

  GitReady := GitDetected();
  PrereqPage := CreateCustomPage(wpSelectDir, 'Host prerequisites',
    'Feather runs commands through Git Bash and records checkpoints with git.');

  PrereqInfo := TNewStaticText.Create(PrereqPage);
  PrereqInfo.Parent := PrereqPage.Surface;
  PrereqInfo.Left := 0;
  PrereqInfo.Top := 0;
  PrereqInfo.Width := PrereqPage.SurfaceWidth;
  PrereqInfo.Height := ScaleY(56);
  PrereqInfo.AutoSize := False;
  PrereqInfo.WordWrap := True;
  if GitReady then
    PrereqInfo.Caption :=
      'Git for Windows is already installed, so the Bash tool and file ' +
      'checkpoints will work. Nothing else is required.'
  else
    PrereqInfo.Caption :=
      'Git for Windows was not found. It supplies both Git Bash (used by the ' +
      'Bash tool) and git (used by file checkpoints). Setup can download it from ' +
      'a mirror in mainland China (USTC, falling back to npmmirror); if that ' +
      'fails, Feather still installs but those two features stay unavailable.';

  PrereqAuto := TNewRadioButton.Create(PrereqPage);
  PrereqAuto.Parent := PrereqPage.Surface;
  PrereqAuto.Left := 0;
  PrereqAuto.Top := PrereqInfo.Top + PrereqInfo.Height + ScaleY(12);
  PrereqAuto.Width := PrereqPage.SurfaceWidth;
  PrereqAuto.Caption := 'Download and install Git for Windows now (recommended)';
  PrereqAuto.Visible := not GitReady;
  PrereqAuto.Checked := not GitReady;

  PrereqBrowser := TNewRadioButton.Create(PrereqPage);
  PrereqBrowser.Parent := PrereqPage.Surface;
  PrereqBrowser.Left := 0;
  PrereqBrowser.Top := PrereqAuto.Top + ScaleY(24);
  PrereqBrowser.Width := PrereqPage.SurfaceWidth;
  PrereqBrowser.Caption :=
    'Open a China mirror (USTC) in my browser, or npmmirror as a fallback';
  PrereqBrowser.Visible := not GitReady;

  PrereqSkip := TNewRadioButton.Create(PrereqPage);
  PrereqSkip.Parent := PrereqPage.Surface;
  PrereqSkip.Left := 0;
  PrereqSkip.Top := PrereqBrowser.Top + ScaleY(24);
  PrereqSkip.Width := PrereqPage.SurfaceWidth;
  PrereqSkip.Caption := 'Skip for now — I will install it myself';
  PrereqSkip.Visible := not GitReady;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  { A machine that already has Git needs no decision here. }
  Result := (PageID = PrereqPage.ID) and GitReady;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  if (CurPageID <> PrereqPage.ID) or GitReady then
    Exit;
  if PrereqBrowser.Checked then
  begin
    ShellExec('open', GitDownloadPageUrl, '', '', SW_SHOWNORMAL, ewNoWait,
      ResultCode);
    Exit;
  end;
  if not PrereqAuto.Checked then
    Exit;
  if InstallGitForWindows() then
    MsgBox('Git for Windows is installed (source: ' + GitSource + ').' + #13#10#13#10 +
      'The Bash tool and file checkpoints are ready.',
      mbInformation, MB_OK)
  else
    MsgBox('Git for Windows could not be installed automatically (no network, ' +
      'cancelled elevation, or the installer failed).' + #13#10#13#10 +
      'Feather itself will still install. Download Git for Windows from either ' +
      'mirror below, then reopen Feather:' + #13#10#13#10 +
      GitDownloadPageUrl + #13#10 +
      GitAltDownloadPageUrl,
      mbInformation, MB_OK);
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
    { The prereq page allows "skip": say plainly, at the end, what will not
      work — before the user meets it as a failed tool call. Silent installs
      must never open a dialog, so they only log. }
    if not GitDetected() then begin
      if WizardSilent() then
        Log('warning: Git for Windows not found; Bash tool and checkpoints unavailable')
      else
        MsgBox('Feather is installed, but Git for Windows was not found.' + #13#10#13#10 +
          'The Bash tool and file checkpoints need it. Download it from either ' +
          'mirror below, then reopen Feather:' + #13#10#13#10 +
          GitDownloadPageUrl + #13#10 +
          GitAltDownloadPageUrl,
          mbInformation, MB_OK);
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
