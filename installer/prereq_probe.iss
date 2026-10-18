; Regression probe for the installer's Git detection rules.
;
; The rules in prereq_git.iss decide whether setup offers to install Git for
; Windows, so a false negative nags every user and a false positive leaves a
; bare machine broken. A real Windows box always has System32\bash.exe (the WSL
; launcher) and usually has Git, so the rules are exercised with synthetic
; inputs as well.
;
;   ISCC installer\prereq_probe.iss
;   target\prereq-probe\feather-prereq-probe.exe /VERYSILENT
;   type %TEMP%\feather-prereq-probe.txt
;
; Every line must read PASS (SKIP lines appear when this host lacks Git).

[Setup]
AppName=Feather prereq probe
AppVersion=0.0
DefaultDirName={tmp}\feather-prereq-probe
OutputDir=..\target\prereq-probe
OutputBaseFilename=feather-prereq-probe
Uninstallable=no
DisableProgramGroupPage=yes
DisableDirPage=yes
DisableWelcomePage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
CreateUninstallRegKey=no
PrivilegesRequired=lowest

[Code]
#include "prereq_git.iss"

var
  Report: String;

procedure Check(const Name: String; Got, Want: Boolean);
begin
  if Got = Want then
    Report := Report + 'PASS ' + Name + #13#10
  else
    Report := Report + 'FAIL ' + Name + ' (wanted ' + IntToStr(Ord(Want)) +
      ', got ' + IntToStr(Ord(Got)) + ')' + #13#10;
end;

function InitializeSetup(): Boolean;
var
  PfGit, Pf32Git, LdGit, GoodPath, BadPath, Pf64Git: String;
begin
  Report := '';
  PfGit := ExpandConstant('{pf}\Git');
  Pf32Git := ExpandConstant('{pf32}\Git');
  LdGit := ExpandConstant('{localappdata}\Programs\Git');
  GoodPath := ExpandConstant('{pf}\Git\cmd');
  Pf64Git := ExpandConstant('{commonpf64}\Git');
  BadPath := ExpandConstant('{sys}');

  { Synthetic: nothing anywhere -> not detected. }
  Check('empty inputs are not a Git install',
    GitDetectedIn('', ''), False);
  Check('bogus registry root is rejected',
    GitDetectedIn('C:\definitely\not\here', ''), False);
  Check('bogus common dirs are rejected',
    GitRootFromCommonDirsIn('C:\definitely\not\here',
      'C:\also\not\here', 'C:\still\not\here') = '', True);

  { The WSL trap: System32 holds bash.exe but no MSYS2, so it must not count. }
  Check('System32 bash.exe is not Git Bash',
    GitOnPathIn(BadPath), False);

  { Empty entries must not mean "current directory". }
  Check('empty PATH entries ignored', GitOnPathIn(';;;'), False);

  { Real machine facts, only asserted when the path actually exists. }
  if FileExists(GoodPath + '\git.exe') then
    Check('real Git on PATH is detected', GitOnPathIn(GoodPath), True)
  else
    Report := Report + 'SKIP real Git PATH check (no Git at ' + GoodPath + ')' + #13#10;
  { The 64-bit install root is where a normal Git for Windows setup lands. }
  if LooksLikeGitRoot(Pf64Git) then
    Check('64-bit Program Files Git root is detected',
      GitDetectedIn(Pf64Git, ''), True)
  else
    Report := Report + 'SKIP 64-bit Git root check' + #13#10;
  if LooksLikeGitRoot(PfGit) then
    Check('real registry/common-dir install is detected',
      GitDetectedIn('', PfGit), True)
  else
    Report := Report + 'SKIP real common-dir check' + #13#10;

  { Registry install, when present, must be recognized on its own. }
  if GitRootFromRegistry() <> '' then
    Check('registry InstallPath is recognized',
      LooksLikeGitRoot(GitRootFromRegistry()), True);

  { End-to-end verdict for this machine. }
  Report := Report + 'GitDetected()=' + IntToStr(Ord(GitDetected())) + #13#10;

  SaveStringToFile(ExpandConstant('{tmp}\..\feather-prereq-probe.txt'),
    Report, False);
  Result := False; { never install }
end;
