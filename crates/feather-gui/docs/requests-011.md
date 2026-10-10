# Feather 0.1.11 — footer/composer and usable slash menus

## Footer spacing

The old session/model/effort slots consumed fixed widths even for short text,
leaving empty space before each dot. Labels now use measured natural widths,
capped/truncated only when needed. Both groups share a 7pt item gap. A fixed
minimum width belongs to the left GROUP, not each label, preserving right-side
alignment without spreading individual items. Geometry regression tests check
visible text gaps at 510/950/1400pt and long/Unicode labels remain bounded.

## Composer

The Send button is gone. Enter still sends/queues through the existing shortcut
registry; Shift+Enter adds a line. Helper text now uses the entire input width.

## Skills

`/skill` or `/skill ` opens an inline metadata-only list. `/skill query` and
`/skill:query` filter name/description. Arrow keys/Tab/Enter or clicking a row
insert `/skill:name `, restore composer focus and move the caret to the end.
Typing the request after that does not re-open the list; Enter expands the body
fresh and submits/queues the request. Discovery runs on a background thread and
refreshes on menu re-entry. Loading/no matches/no skills and metadata errors are
shown instead of a silent missing dropdown.

Actual read-only metadata discovery found three local skills: docx/pptx/xlsx.
Bodies are not loaded for listing. Metadata overhead is now about 760 estimated
tokens; the old 0-skill overhead report was a measurement before these folders
were present, not a permanent overhead guarantee.

## Rewind

`/rewind` and `/rewind ` show saved turn numbers paired with user requests.
Labels come from a cloned shared redb snapshot handle, not agent.try_lock(),
which used to silently cache an empty list while a response held the mutex.
The handle is attached on both startup auto-resume and later agent installation,
invalidated at durable commit/run completion, and released before session rebuild
or switching. Errors/loading/empty states are visible. Filtering uses turn number
or request text; arrows/Enter/click selects the target (latest 128 labels).
Listing is allowed while busy; executing rewind still requires stopping the
response. A fresh session has no committed turns to restore.

## Validation

Final tests: 159 GUI tests passed; workspace suites/check/diff check passed.
Release CPU/DX12 maximize/minimize/input/close/header-X and automatic renderer
restart passed; minimized close with a 60s blocking worker also passed.
Workspace tests/check/whitespace passed before release; GUI tests include equal
footer spacing, no Send button, skill body-lazy discovery/reopen, keyboard
selection followed by real typed request/Enter queue, mouse selection for both
menus, and rewind listing while the real agent mutex is held followed by actual
rewind. Screenshot fixtures are synthetic and isolated from user credentials/
transcripts. DeepSeek reviewed skills011-fixed.png: 8/10 PASS. The earlier
multi-screenshot review returned REVISE; the PASS applies to this final skills
screen, not a fabricated global review verdict.

Release installer allowlist remains just two executables and third-party notices.
Local replacement must preserve the user's running GUI; close/reopen is needed
to activate the new image. No session/config/credential reset is performed.
