; Regression probe for the installer's Git detection and release lookup rules.
;
; prereq_git.iss decides (a) whether setup offers to install Git for Windows and
; (b) which file it downloads. A false negative nags every user, a false
; positive leaves a bare machine broken, and picking the wrong asset installs a
; stripped archive (MinGit) or a portable copy instead of Git. A real Windows
; box always has System32\bash.exe (the WSL launcher) and usually has Git, so the
; rules are exercised with synthetic inputs as well.
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
procedure CheckEq(const Name, Got, Want: String);
begin
  if Got = Want then
    Report := Report + 'PASS ' + Name + #13#10
  else
    Report := Report + 'FAIL ' + Name + ' (wanted "' + Want + '", got "' +
      Got + '")' + #13#10;
end;

{ Release-lookup rules, pinned against captures of the real listings. }
procedure CheckReleaseLookup();
var
  UstcBody, NpmRoot, NpmDir: String;
begin
  UstcBody :=
    '<tr><td><a href="/github-release/git-for-windows/git/LatestRelease/' +
    'Git-2.56.0.2-64-bit.exe">Git-2.56.0.2-64-bit.exe</a></td></tr>' + #10 +
    '<tr><td><a href="/github-release/git-for-windows/git/LatestRelease/' +
    'Git-2.56.0.2-arm64.exe">Git-2.56.0.2-arm64.exe</a></td></tr>' + #10 +
    '<tr><td><a href="/github-release/git-for-windows/git/LatestRelease/' +
    'PortableGit-2.56.0.2-64-bit.7z.exe">PortableGit-2.56.0.2-64-bit.7z.exe</a>' +
    '</td></tr>' + #10 +
    '<tr><td><a href="/github-release/git-for-windows/git/LatestRelease/' +
    'MinGit-2.56.0.2-64-bit.zip">MinGit-2.56.0.2-64-bit.zip</a></td></tr>';
  NpmRoot :=
    '[{"category":"git-for-windows","name":"v2.39.4.windows.1/","type":"dir",' +
    '"url":"https://registry.npmmirror.com/-/binary/git-for-windows/v2.39.4.windows.1/"},' +
    '{"category":"git-for-windows","name":"v2.50.0-rc0.windows.1/","type":"dir",' +
    '"url":"https://registry.npmmirror.com/-/binary/git-for-windows/v2.50.0-rc0.windows.1/"},' +
    '{"category":"git-for-windows","name":"v2.55.0.windows.5/","type":"dir",' +
    '"url":"https://registry.npmmirror.com/-/binary/git-for-windows/v2.55.0.windows.5/"},' +
    '{"category":"git-for-windows","name":"v2.56.0.windows.2/","type":"dir",' +
    '"url":"https://registry.npmmirror.com/-/binary/git-for-windows/v2.56.0.windows.2/"}]';
  NpmDir :=
    '[{"category":"git-for-windows","name":"Git-2.56.0.2-64-bit.exe","type":"file",' +
    '"size":66763664,"url":"https://registry.npmmirror.com/-/binary/' +
    'git-for-windows/v2.56.0.windows.2/Git-2.56.0.2-64-bit.exe"},' +
    '{"category":"git-for-windows","name":"Git-2.56.0.2-64-bit.tar.bz2","type":"file"}]';
  { The installer asset, never MinGit or PortableGit. }
  CheckEq('USTC index yields the 64-bit installer',
    PickReleaseExeName(UstcBody), 'Git-2.56.0.2-64-bit.exe');
  CheckEq('npmmirror dir yields the 64-bit installer',
    PickReleaseExeName(NpmDir), 'Git-2.56.0.2-64-bit.exe');

  { A file name is never invented from an empty or unrelated body. }
  CheckEq('empty index yields nothing', PickReleaseExeName(''), '');
  CheckEq('index without a Git installer yields nothing',
    PickReleaseExeName('<a href="/nope/Other-1-64-bit.exe">x</a>'), '');

  { Newest stable release, ignoring the rc preview. }
  CheckEq('npmmirror picks the newest stable release',
    PickNewestStableVersionDir(NpmRoot), 'v2.56.0.windows.2/');
  CheckEq('empty listing yields nothing', PickNewestStableVersionDir(''), '');
  Check('version order compares numerically, not as text',
    VersionKey('v2.56.0.windows.2/') > VersionKey('v2.39.4.windows.1/'), True);
  Check('a release candidate sorts below its release',
    VersionKey('v2.55.0.windows.5/') > VersionKey('v2.50.0-rc0.windows.1/'), True);

  { URL joining must not double or drop the separator. }
  CheckEq('base without slash joins cleanly',
    JoinUrl('https://m.example/rel', 'Git-1-64-bit.exe'),
    'https://m.example/rel/Git-1-64-bit.exe');
  CheckEq('absolute name joins cleanly',
    JoinUrl('https://m.example/rel/', '/Git-1-64-bit.exe'),
    'https://m.example/rel/Git-1-64-bit.exe');
end;

function InitializeSetup(): Boolean;
var
  PfGit, Pf32Git, LdGit, GoodPath, BadPath, Pf64Git, LiveUrl: String;
begin
  Report := '';
  PfGit := ExpandConstant('{pf}\Git');
  Pf32Git := ExpandConstant('{pf32}\Git');
  LdGit := ExpandConstant('{localappdata}\Programs\Git');
  GoodPath := ExpandConstant('{pf}\Git\cmd');
  Pf64Git := ExpandConstant('{commonpf64}\Git');
  BadPath := ExpandConstant('{sys}');

  CheckReleaseLookup();

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

  { Live check: resolve against the real mirrors. Only the small index pages are
    fetched -- never the installer -- so this stays fast. A network that cannot
    reach a China mirror is reported as SKIP, not FAIL. }
  LiveUrl := ResolveGitInstallerUrl();
  if LiveUrl = '' then
    Report := Report + 'SKIP live mirror resolution (no network to any host)' + #13#10
  else
  begin
    Check('live resolution returned the 64-bit installer',
      (Pos('/git-', Lowercase(LiveUrl)) > 0) and
      (Pos('-64-bit.exe', Lowercase(LiveUrl)) > 0) and
      (Copy(Lowercase(LiveUrl), 1, 8) = 'https://'), True);
    Report := Report + 'LIVE source=' + GitSource + ' url=' + LiveUrl + #13#10;
  end;

  { The fallback mirror is only exercised when USTC is unreachable, so check it
    directly: a broken fallback would leave China users with no automatic path. }
  LiveUrl := ResolveFromNpmMirror();
  if LiveUrl = '' then
    Report := Report + 'SKIP live npmmirror resolution (unreachable)' + #13#10
  else
  begin
    Check('live npmmirror resolution returned the 64-bit installer',
      (Pos('/git-', Lowercase(LiveUrl)) > 0) and
      (Pos('-64-bit.exe', Lowercase(LiveUrl)) > 0) and
      (Copy(Lowercase(LiveUrl), 1, 8) = 'https://'), True);
    Report := Report + 'LIVE npmmirror url=' + LiveUrl + #13#10;
  end;

  { End-to-end verdict for this machine. }
  Report := Report + 'GitDetected()=' + IntToStr(Ord(GitDetected())) + #13#10;

  SaveStringToFile(ExpandConstant('{tmp}\..\feather-prereq-probe.txt'),
    Report, False);
  Result := False; { never install }
end;
