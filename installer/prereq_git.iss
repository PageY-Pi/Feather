{ ---------------------------------------------------------------------------
  Git for Windows detection and release lookup, shared by installer\feather.iss
  and the probe in installer\prereq_probe.iss so the two cannot drift apart.

  Feather runs model-issued commands through Git Bash and drives file
  checkpoints with git. Neither is bundled: shipping them would add ~60 MB and
  pull a GPLv2 redistribution obligation into this installer. A bare Windows
  machine can therefore finish setup with an unusable Bash tool, so the wizard
  detects Git for Windows and offers to install it.

  Downloads resolve through mirrors hosted in mainland China first: the official
  GitHub release host is frequently unreachable there, which would turn the
  "install it for me" path into a slow timeout. The parsers below are pure
  string functions -- the *In variants take their inputs explicitly -- so the
  probe can pin them against captured listings instead of the live network.
--------------------------------------------------------------------------- }

{ ---------------------------------------------------------------------------
  Detection
--------------------------------------------------------------------------- }

const
  { Mainland-China mirrors first. GitHub's release host is routinely blocked or
    throttled there, which turns the automatic install into a long timeout. Both
    mirrors below publish the official Git for Windows assets over HTTPS. }
  UstcIndexUrl =
    'https://mirrors.ustc.edu.cn/github-release/git-for-windows/git/LatestRelease/';
  UstcBaseUrl =
    'https://mirrors.ustc.edu.cn/github-release/git-for-windows/git/LatestRelease/';
  NpmMirrorRootUrl = 'https://registry.npmmirror.com/-/binary/git-for-windows/';
  { Browser fallbacks for a machine where the automatic path fails. }
  GitDownloadPageUrl =
    'https://mirrors.ustc.edu.cn/github-release/git-for-windows/git/LatestRelease/';
  GitAltDownloadPageUrl =
    'https://registry.npmmirror.com/binary.html?path=git-for-windows/';
  { Outside China the mirrors may be unreachable while GitHub is not. }
  GitLatestApiUrl = 'https://api.github.com/repos/git-for-windows/git/releases/latest';

var
  { Which host supplied the download; shown to the user for transparency. }
  GitSource: String;
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

{ Git for Windows records where it went, so this is the authoritative answer
  even when PATH is stale or the user chose the "Git Bash only" PATH option. }
function GitRootFromRegistry(): String;
var
  Value: String;
begin
  Result := '';
  if RegQueryStringValue(HKLM, 'SOFTWARE\GitForWindows', 'InstallPath', Value) then
    Result := Value
  else if RegQueryStringValue(HKLM32, 'SOFTWARE\GitForWindows', 'InstallPath', Value) then
    Result := Value
  else if RegQueryStringValue(HKCU, 'SOFTWARE\GitForWindows', 'InstallPath', Value) then
    Result := Value;
end;

function LooksLikeGitRoot(const Dir: String): Boolean;
var
  Root: String;
begin
  Result := False;
  if Dir = '' then
    Exit;
  Root := RemoveBackslashUnlessRoot(Dir);
  Result := FileExists(Root + '\bin\bash.exe') and
    (FileExists(Root + '\cmd\git.exe') or FileExists(Root + '\bin\git.exe'));
end;

function GitRootFromCommonDirsIn(const PfDir, Pf32Dir, LdDir: String): String;
begin
  Result := '';
  if LooksLikeGitRoot(PfDir) then begin Result := PfDir; Exit; end;
  if LooksLikeGitRoot(Pf32Dir) then begin Result := Pf32Dir; Exit; end;
  if LooksLikeGitRoot(LdDir) then begin Result := LdDir; Exit; end;
end;

function GitRootFromCommonDirs(): String;
begin
  Result := GitRootFromCommonDirsIn(
    ExpandConstant('{pf}\Git'),
    ExpandConstant('{pf32}\Git'),
    ExpandConstant('{localappdata}\Programs\Git'));
end;

{ A PATH entry only counts when it really holds MSYS2 bash: Windows also ships
  a WSL launcher named bash.exe in System32, and that is not a substitute. }
function GitOnPathIn(const PathValue: String): Boolean;
var
  Parts: TArrayOfString;
  I: Integer;
  Dir: String;
begin
  Result := False;
  Parts := SplitString(PathValue, ';');
  for I := 0 to GetArrayLength(Parts) - 1 do
  begin
    Dir := RemoveBackslashUnlessRoot(Trim(Parts[I]));
    if Dir = '' then
      Continue;
    if FileExists(Dir + '\git.exe') then
    begin
      Result := True;
      Exit;
    end;
    if FileExists(Dir + '\bash.exe') and
       (FileExists(Dir + '\msys-2.0.dll') or
        FileExists(Dir + '\..\usr\bin\msys-2.0.dll')) then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

function GitOnPath(): Boolean;
begin
  Result := GitOnPathIn(GetEnv('PATH'));
end;

{ Testable core: no environment access at all. }
function GitDetectedIn(const RegistryRoot, PathValue: String): Boolean;
begin
  Result := LooksLikeGitRoot(RegistryRoot) or
    (GitRootFromCommonDirs() <> '') or GitOnPathIn(PathValue);
end;

function GitDetected(): Boolean;
begin
  Result := GitDetectedIn(GitRootFromRegistry(), GetEnv('PATH'));
end;

{ ---------------------------------------------------------------------------
  Release lookup

  Two mirror shapes are supported, both HTTPS:

  * a flat index (the USTC github-release mirror keeps a stable "LatestRelease"
    directory holding exactly the current assets);
  * a versioned listing (npmmirror keeps one directory per release, newest last).

  Filenames are derived, never guessed: the installer must pick the 64-bit
  installer and must not fall for MinGit (a stripped archive) or PortableGit.
--------------------------------------------------------------------------- }

function IsNameBoundary(C: Char): Boolean;
begin
  Result := (C = '/') or (C = '\') or (C = '"') or (C = '''') or
    (C = '>') or (C = '<') or (C = '=') or (C = ' ') or
    (C = #9) or (C = #10) or (C = #13);
end;

{ First "Git-<version>-64-bit.exe" that starts a filename. Anchoring on the
  boundary is what rejects MinGit-... and PortableGit-...: their names contain
  "Git-" but do not begin with it. }
function PickReleaseExeName(const Body: String): String;
var
  Needle, Name: String;
  From, At, Start: Integer;
begin
  Result := '';
  Needle := '-64-bit.exe';
  From := 1;
  while True do
  begin
    At := Pos(Needle, Copy(Body, From, Length(Body) - From + 1));
    if At = 0 then
      Exit;
    At := At + From - 1;
    Start := At;
    while (Start > 1) and (not IsNameBoundary(Body[Start - 1])) do
      Start := Start - 1;
    Name := Copy(Body, Start, At - Start + Length(Needle));
    if (Length(Name) >= 4) and (CompareText(Copy(Name, 1, 4), 'Git-') = 0) then
    begin
      Result := Name;
      Exit;
    end;
    From := At + Length(Needle);
  end;
end;

{ Pack the numeric groups of a release directory into one comparable integer:
  v2.56.0.windows.2 -> 2*1e9 + 56*1e6 + 0*1e3 + 2. Pre-release names never
  reach this function, and groups are capped so the product cannot overflow. }
function VersionKey(const DirName: String): Int64;
var
  Text: String;
  I, Part, Digit: Integer;
  Value: Int64;
  InNumber: Boolean;
begin
  Result := 0;
  Text := DirName;
  if (Length(Text) > 0) and ((Text[1] = 'v') or (Text[1] = 'V')) then
    Delete(Text, 1, 1);
  Part := 0;
  Value := 0;
  InNumber := False;
  for I := 1 to Length(Text) do
  begin
    if (Text[I] >= '0') and (Text[I] <= '9') then
    begin
      Digit := Ord(Text[I]) - Ord('0');
      if Value < 1000 then
        Value := Value * 10 + Digit;
      InNumber := True;
    end
    else if InNumber then
    begin
      if Part = 0 then Result := Result + Value * 1000000000
      else if Part = 1 then Result := Result + Value * 1000000
      else if Part = 2 then Result := Result + Value * 1000
      else if Part = 3 then Result := Result + Value;
      Part := Part + 1;
      Value := 0;
      InNumber := False;
      if Part >= 4 then
        Exit;
    end;
  end;
  if InNumber then
  begin
    if Part = 0 then Result := Result + Value * 1000000
    else if Part = 1 then Result := Result + Value * 1000
    else if Part = 2 then Result := Result + Value;
  end;
end;

{ Newest stable release directory in a versioned mirror listing, e.g.
  "v2.56.0.windows.2/". Pre-releases and the rolling "untagged-" build are
  ignored: setup must install a released Git, never a preview. }
function PickNewestStableVersionDir(const Body: String): String;
var
  Needle, Name, Lower, Best: String;
  From, At, Quote: Integer;
  Key, BestKey: Int64;
begin
  Result := '';
  Needle := '"name":"v';
  From := 1;
  Best := '';
  BestKey := -1;
  while True do
  begin
    At := Pos(Needle, Copy(Body, From, Length(Body) - From + 1));
    if At = 0 then
      Break;
    At := At + From - 1;
    { Name content starts right after '"name":"'. }
    Quote := Pos('"', Copy(Body, At + 8, Length(Body) - (At + 7)));
    if Quote = 0 then
      Break;
    Name := Copy(Body, At + 8, Quote - 1);
    Lower := Lowercase(Name);
    if (Pos('.windows.', Lower) > 0) and
       (Pos('-rc', Lower) = 0) and
       (Pos('prerelease', Lower) = 0) then
    begin
      Key := VersionKey(Name);
      if Key > BestKey then
      begin
        BestKey := Key;
        Best := Name;
      end;
    end;
    From := At + 8;
  end;
  Result := Best;
end;

{ Join a mirror base and a filename without doubling the separator. }
function JoinUrl(const Base, Name: String): String;
begin
  Result := Base;
  if (Length(Result) > 0) and (Result[Length(Result)] <> '/') then
    Result := Result + '/';
  if (Length(Name) > 0) and (Name[1] = '/') then
    Result := Result + Copy(Name, 2, Length(Name))
  else
    Result := Result + Name;
end;
{ Fetch a URL into a temp file and return its text, or '' on any failure. }
function FetchText(const Url, FileName: String): String;
var
  Data: AnsiString;
  Path: String;
begin
  Result := '';
  Path := ExpandConstant('{tmp}\') + FileName;
  if FileExists(Path) then
    DeleteFile(Path);
  try
    DownloadTemporaryFile(Url, FileName, '', nil);
  except
    Log('prereq: fetch failed for ' + Url);
    Exit;
  end;
  if LoadStringFromFile(Path, Data) then
    Result := String(Data);
end;

{ USTC mirror: one stable index page names the current assets. }
function ResolveFromUstc(): String;
var
  Name: String;
begin
  Result := '';
  Name := PickReleaseExeName(FetchText(UstcIndexUrl, 'git-ustc.html'));
  if Name <> '' then
  begin
    Result := JoinUrl(UstcBaseUrl, Name);
    GitSource := '清华大学开源软件镜像站 (USTC)';
  end;
end;

{ npmmirror: one directory per release, so pick the newest stable one first. }
function ResolveFromNpmMirror(): String;
var
  Dir, Name: String;
begin
  Result := '';
  Dir := PickNewestStableVersionDir(FetchText(NpmMirrorRootUrl, 'git-npm.json'));
  if Dir = '' then
    Exit;
  Name := PickReleaseExeName(FetchText(JoinUrl(NpmMirrorRootUrl, Dir), 'git-npm-ver.json'));
  if Name = '' then
    Exit;
  Result := JoinUrl(JoinUrl(NpmMirrorRootUrl, Dir), Name);
  GitSource := 'npmmirror (阿里巴巴镜像)';
end;

{ Official GitHub release, for machines that can reach it. }
function ResolveFromGithub(): String;
var
  Text, Rest, Needle, Url: String;
  At, Quote: Integer;
begin
  Result := '';
  Text := FetchText(GitLatestApiUrl, 'git-latest.json');
  if Text = '' then
    Exit;
  Needle := '"browser_download_url":"';
  Rest := Text;
  while True do
  begin
    At := Pos(Needle, Rest);
    if At = 0 then
      Exit;
    Rest := Copy(Rest, At + Length(Needle), Length(Rest));
    Quote := Pos('"', Rest);
    if Quote = 0 then
      Exit;
    Url := Copy(Rest, 1, Quote - 1);
    if (Pos('-64-bit.exe', Url) > 0) and (Pos('.sig', Url) = 0) then
    begin
      Result := Url;
      GitSource := 'GitHub';
      Exit;
    end;
    Rest := Copy(Rest, Quote, Length(Rest));
  end;
end;

{ China-first: both mirrors are reachable on mainland networks where the
  official host is not. Each attempt is independent, so one blocked host only
  costs that attempt. Empty result leaves the browser path to the user. }
function ResolveGitInstallerUrl(): String;
begin
  GitSource := '';
  Result := ResolveFromUstc();
  if Result = '' then
    Result := ResolveFromNpmMirror();
  if Result = '' then
    Result := ResolveFromGithub();
end;
