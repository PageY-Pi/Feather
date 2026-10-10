# Windows Bash and Chinese tool-output investigation

## Evidence from the current workspace

The local redb audit for `C:/Users/Page/Desktop/feather` contained one conversation
(`s01a11a6ab4ed0000`) with 111 matched Bash results and 19 failed results.
No pending call was found. The audit did not replay any historical command.

The implementation called `cmd.exe /C` on Windows even though the tool was named
`bash`. Many failed commands used Bash syntax: semicolons, POSIX `/c/...` paths,
`2>/dev/null`, `$USERPROFILE`, `for ...; do ...; done`, and Unix utilities in
pipelines. cmd parsed these with different quoting/separator/path semantics.
Examples included the reported read-only command:

```sh
cd /c/Users/Page/.feather/sessions && pwd && ls | head -3
```

Its recorded result was exit 1, with a Windows path error already corrupted into
replacement characters. Other failures had genuine invalid paths, a malformed
path token, nonexistent files, formatting-check differences, or grep's ordinary
exit 1 for no matches. Fixing the shell must not convert these genuine nonzero
exits into success.

## Fix

Both model BashTool and RPC Bash now use the same actual Bash discovery:

- Windows: locate Git Bash/MSYS2 using PATH, Git-relative installation paths and
  standard installation directories. Reject the System32 WSL launcher and cmd.
- Optional `FEATHER_BASH` points to an existing runtime; absent runtime returns an
  actionable error rather than silently running a different shell.
- Supply the runtime's `usr/bin`, `mingw64/bin` and `bin` on PATH, preserving the
  inherited native-tool paths. This supports Explorer-launched GUI processes,
  not just a GUI launched from Git Bash.
- Run `--noprofile --norc -c`, remove BASH_ENV/ENV injection, and use UTF-8 locale.
  Each command starts in the configured workspace; `cd` does not persist.
- Keep real exit statuses. Drain both output pipes concurrently; keep at most
  8 MiB per pipe but continue draining after the cap. One deadline covers waiting
  for process exit and pipe EOF. Cancel/timeout terminates the Windows process tree.

## Chinese display

The old tool cards already registered Microsoft YaHei as a monospace fallback.
On this machine both old and new font definitions produce real Chinese glyphs
and nonempty egui meshes. Cascadia does not claim the tested Chinese codepoints.
There was no evidence for an empty-glyph monospace font blocking fallback.

The observed corruption was upstream: native Windows error output (OEM code page,
CP936 on this machine) was passed through lossy UTF-8 decoding. Once U+FFFD is
stored in redb, changing the font cannot recover the lost original bytes.

New output is decoded as UTF-8 first; invalid UTF-8 native output falls back to the
Windows OEM code page. RPC streaming retains incomplete UTF-8 bytes separately
per pipe so split Chinese characters do not become replacement glyphs. Expanded
cards and argument summaries use additional installed CJK fallback choices while
preserving the Latin monospace face for code alignment. Historical corrupted
messages remain unchanged; rerun read-only commands to obtain readable output.
Mixed encodings within one native pipe cannot be inferred perfectly; programs
should emit UTF-8 where configurable.

## Validation

Regression tests cover POSIX loops/separators/redirects/pipelines, session directory
navigation, UTF-8 stdout and stderr, native CP936 decode, WSL rejection, nonzero
exit preservation, closed abort channels, timeout, output beyond the capture cap,
split UTF-8 RPC chunks and actual Chinese glyph/expanded-card galleys.
A release smoke used a minimal Windows PATH (Git cmd, System32, cargo; no Bash
usr/bin), performed a synthetic tool-call HTTP roundtrip, confirmed Chinese
stdout/stderr with exit 0 reached the parent request, and reopened the redb
conversation in the GUI. Tool argument Chinese was verified visually.

Local utilities:

```sh
cargo run -p feather --example audit_bash
cargo run -p feather --example bash_smoke
```

The audit utility prints bounded failed-command excerpts. Review it locally;
do not publish transcripts containing private commands or outputs.
