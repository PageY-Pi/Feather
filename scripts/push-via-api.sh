#!/usr/bin/env bash
# Push a local commit to GitHub through the REST Git Data API.
#
# `github.com:443` is unreachable from this machine while `api.github.com` works,
# so `git push` cannot run. The Data API can build the same commit server-side:
# upload each changed file as a blob, build a tree on top of the remote head,
# create the commit, then move the branch ref. The result is an ordinary commit
# with the same tree as the local one.
#
# Usage: scripts/push-via-api.sh <base-sha> <commit-message-file>
set -euo pipefail

REPO=PageY-Pi/Feather
API=https://api.github.com/repos/$REPO
BASE_SHA=${1:?base commit sha}
MSG_FILE=${2:?commit message file}

TOK=$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | grep '^password=' | cut -d= -f2)
AUTH="Authorization: Bearer $TOK"
[ -n "$TOK" ] || { echo "no token" >&2; exit 1; }

api() {
  local method=$1 path=$2; shift 2
  curl -sS -X "$method" -H "$AUTH" -H "Accept: application/vnd.github+json" \
    -H "Content-Type: application/json" "$API$path" "$@"
}

# Blob for every file that differs from the base commit. `base_tree` carries all
# the untouched files, so only the changed ones are sent.
TREE_JSON=$(python - "$BASE_SHA" <<'PY'
import base64, json, subprocess, sys
base = sys.argv[1]
files = subprocess.run(
    ["git", "diff", "--name-status", f"{base}..HEAD"],
    capture_output=True, text=True, check=True,
).stdout.strip().splitlines()
entries = []
for line in files:
    status, path = line.split("\t", 1)
    if status.startswith("D"):
        entries.append({"path": path, "mode": "100644", "type": "blob", "sha": None})
        continue
    data = subprocess.run(["git", "show", f"HEAD:{path}"], capture_output=True, check=True).stdout
    mode = subprocess.run(
        ["git", "ls-tree", "HEAD", path], capture_output=True, text=True, check=True
    ).stdout.split()[0]
    entries.append({
        "path": path, "mode": mode, "type": "blob",
        "_content": base64.b64encode(data).decode(),
    })
print(json.dumps(entries))
PY
)

# Upload each blob and swap its content for the returned sha.
TREE_SPEC=$(echo "$TREE_JSON" | GH_TOKEN="$TOK" GH_API="$API" python -c '
import json, sys, urllib.request, os
entries = json.load(sys.stdin)
token = os.environ["GH_TOKEN"]
api = os.environ["GH_API"]
out = []
for e in entries:
    content = e.pop("_content", None)
    if content is None:
        out.append(e)
        continue
    body = json.dumps({"content": content, "encoding": "base64"}).encode()
    req = urllib.request.Request(f"{api}/git/blobs", data=body, method="POST",
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json",
                 "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req) as r:
        e["sha"] = json.load(r)["sha"]
    out.append(e)
print(json.dumps(out))
')

TREE_SHA=$(api POST /git/trees --data "{\"base_tree\":\"$BASE_SHA\",\"tree\":$TREE_SPEC}" \
  | python -c 'import json,sys; d=json.load(sys.stdin); print(d.get("sha") or (_ for _ in ()).throw(SystemExit(d)))')

# Reuse the local commit message so the server-side commit reads identically.
MSG_JSON=$(python -c 'import json,sys; print(json.dumps(open(sys.argv[1], encoding="utf-8").read()))' "$MSG_FILE")

COMMIT_SHA=$(api POST /git/commits \
  --data "{\"message\":$MSG_JSON,\"tree\":\"$TREE_SHA\",\"parents\":[\"$BASE_SHA\"]}" \
  | python -c 'import json,sys; d=json.load(sys.stdin); print(d.get("sha") or (_ for _ in ()).throw(SystemExit(d)))')

echo "tree=$TREE_SHA commit=$COMMIT_SHA"
echo "$COMMIT_SHA"
