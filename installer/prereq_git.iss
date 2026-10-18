{ ---------------------------------------------------------------------------
  Git for Windows detection, shared by installer\feather.iss and the probe in
  installer\prereq_probe.iss so the two can never drift apart.

  Feather runs model-issued commands through Git Bash and drives file
  checkpoints with git. Neither is bundled: shipping them would add ~60 MB and
  pull a GPLv2 redistribution obligation into this installer. A bare Windows
  machine can therefore finish setup with an unusable Bash tool, so the wizard
  detects Git for Windows and offers to install it.

  The *In variants take their inputs explicitly. That is what makes the rules
  testable: a real Windows box always has System32\bash.exe (WSL) and usually
  has Git, so passing synthetic values is the only way to pin both answers.
--------------------------------------------------------------------------- }

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
