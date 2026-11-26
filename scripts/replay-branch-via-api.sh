#!/usr/bin/env bash
# Replay a local branch onto GitHub through the REST Git Data API.
#
# Why not `git push`: `github.com:443` is unreachable from this machine while
# `api.github.com` is reachable (verified through the system proxy at
# 127.0.0.1:12334). `scripts/push-via-api.sh` handles a single commit; this branch
# has 35, so each is replayed in order.
#
# Why replays rather than one squashed commit: the server-side history should keep
# the commits a normal push would have created, so `git log` on GitHub reads the
# same as the local one. Each step takes its parent from the *server* and its
# changed blobs from the matching *local* commit.
#
# The four arguments matter. Local and server history are the same content under
# different SHAs: the earlier push went through the API too, so server commits are
# not the local ones. `local-base` is the local commit whose tree equals the
# server head's tree, and it is what the first replayed commit is diffed against.
# Assuming the first commit reuses the base tree is wrong — it usually has changes
# of its own, and doing so silently drops them.
#
# Usage: scripts/replay-branch-via-api.sh \
#          <server-base-sha> <local-base-sha> <local-first> <local-last>
set -euo pipefail

REPO=PageY-Pi/Feather
API=https://api.github.com/repos/$REPO
PROXY=${FEATHER_PROXY:-http://127.0.0.1:12334}

BASE=${1:?server-side base commit sha}
LOCAL_BASE=${2:?local commit whose tree equals the server base tree}
FIRST=${3:?first local commit to replay}
LAST=${4:?last local commit to replay}

TOK=$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null |
  grep '^password=' | cut -d= -f2)
[ -n "$TOK" ] || { echo "no token" >&2; exit 1; }

api() {
  local method=$1 path=$2
  shift 2
  curl -sS -x "$PROXY" -X "$method" \
    -H "Authorization: Bearer $TOK" \
    -H "Accept: application/vnd.github+json" \
    -H "Content-Type: application/json" \
    "$API$path" "$@"
}

server_tree() {
  api GET "/git/commits/$1" |
    python -c 'import json,sys; print(json.load(sys.stdin)["tree"]["sha"])'
}

# path -> mode for every file in the server's base tree, fetched once. Only the
# executable bit matters here, and only `scripts/push-via-api.sh` disagrees, but
# reading it from the server keeps the rule general.
SERVER_MODES=$(api GET "/git/trees/$(server_tree "$BASE")?recursive=1" |
  python -c 'import json,sys
d=json.load(sys.stdin)
print(json.dumps({e["path"]: e["mode"] for e in d.get("tree") or [] if e["type"]=="blob"}))')

# Upload only the blobs that differ from the previous local commit, then build the
# server tree on top of the previous server commit. Re-sending the whole tree per
# commit would be 35 x 211 blobs for 78 changed files.
blob_tree_spec() {
  FEATHER_TOK="$TOK" FEATHER_API="$API" FEATHER_PROXY="$PROXY" \
    FEATHER_SERVER_MODES="$SERVER_MODES" \
    python - "$1" "$2" <<'PY'
import base64, json, os, subprocess, sys, urllib.request

prev, sha = sys.argv[1], sys.argv[2]
token = os.environ["FEATHER_TOK"]
api_root = os.environ["FEATHER_API"]
proxy = os.environ["FEATHER_PROXY"]
# path -> mode as the server already stores it, so a file's executable bit is not
# rewritten just because the local checkout disagrees.
server_modes = json.loads(os.environ["FEATHER_SERVER_MODES"])
opener = urllib.request.build_opener(
    urllib.request.ProxyHandler({"http": proxy, "https": proxy})
)

out = subprocess.run(
    ["git", "diff", "--name-status", f"{prev}..{sha}"],
    capture_output=True, text=True, check=True,
).stdout.strip()

entries = []
for line in [l for l in out.splitlines() if l]:
    status, path = line.split("\t", 1)
    if status.startswith("D"):
        entries.append({"path": path, "mode": "100644", "type": "blob", "sha": None})
        continue
    data = subprocess.run(
        ["git", "show", f"{sha}:{path}"], capture_output=True, check=True
    ).stdout
    # Keep the server's mode for a path that already exists there. The two histories
    # disagree on exactly one bit — `scripts/push-via-api.sh` is 100644 locally and
    # 100755 on the server, with identical content — and that alone makes every tree
    # SHA differ. Uploading the local mode would rewrite the bit on each commit and
    # make the tree comparison below impossible.
    mode = server_modes.get(path) or subprocess.run(
        ["git", "ls-tree", sha, path], capture_output=True, text=True, check=True
    ).stdout.split()[0]
    body = json.dumps(
        {"content": base64.b64encode(data).decode(), "encoding": "base64"}
    ).encode()
    req = urllib.request.Request(
        f"{api_root}/git/blobs", data=body, method="POST",
        headers={"Authorization": f"Bearer {token}",
                 "Content-Type": "application/json",
                 "Accept": "application/vnd.github+json"},
    )
    with opener.open(req) as r:
        entries.append({"path": path, "mode": mode, "type": "blob",
                        "sha": json.load(r)["sha"]})
print(json.dumps(entries))
PY
}

parent=$BASE
prev=$LOCAL_BASE
for sha in $(git rev-list --reverse "$FIRST^..$LAST"); do
  msg_json=$(git log -1 --format=%B "$sha" |
    python -c 'import json,sys; print(json.dumps(sys.stdin.read()))')

  spec=$(blob_tree_spec "$prev" "$sha")
  if [ "$spec" = "[]" ]; then
    # Nothing changed relative to the previous local commit, so the parent's tree
    # is already correct and no API call is needed.
    tree_sha=$(server_tree "$parent")
  else
    tree_sha=$(api POST /git/trees \
      --data "{\"base_tree\":\"$(server_tree "$parent")\",\"tree\":$spec}" |
      python -c 'import json,sys; d=json.load(sys.stdin); print(d.get("sha") or (_ for _ in ()).throw(SystemExit(d)))')
  fi

  parent=$(api POST /git/commits \
    --data "{\"message\":$msg_json,\"tree\":\"$tree_sha\",\"parents\":[\"$parent\"]}" |
    python -c 'import json,sys; d=json.load(sys.stdin); print(d.get("sha") or (_ for _ in ()).throw(SystemExit(d)))')

  # Fail loudly on any content drift: a wrong tree means later commits build on the
  # wrong base and the error compounds. Content, not tree SHA, because the one
  # executable-bit disagreement between the histories is deliberately preserved.
  if ! FEATHER_API="$API" FEATHER_PROXY="$PROXY" FEATHER_TOK="$TOK" FEATHER_TREE="$tree_sha" \
     python - "$sha" <<'PY'
import json, os, subprocess, sys, urllib.request

sha = sys.argv[1]
opener = urllib.request.build_opener(urllib.request.ProxyHandler(
    {"http": os.environ["FEATHER_PROXY"], "https": os.environ["FEATHER_PROXY"]}))
req = urllib.request.Request(
    f"{os.environ['FEATHER_API']}/git/trees/{os.environ['FEATHER_TREE']}?recursive=1",
    headers={"Authorization": f"Bearer {os.environ['FEATHER_TOK']}",
             "Accept": "application/vnd.github+json"})
with opener.open(req) as r:
    served = {e["path"]: e["sha"] for e in json.load(r).get("tree") or [] if e["type"] == "blob"}
local = {}
out = subprocess.run(["git", "ls-tree", "-r", sha], capture_output=True, text=True, check=True).stdout
for line in out.splitlines():
    meta, path = line.split("\t", 1)
    local[path] = meta.split()[2]
if served != local:
    only_s = sorted(set(served) - set(local))[:5]
    only_l = sorted(set(local) - set(served))[:5]
    diff = [p for p in served if p in local and served[p] != local[p]][:5]
    print(f"CONTENT MISMATCH at {sha}: only-server={only_s} only-local={only_l} differ={diff}",
          file=sys.stderr)
    sys.exit(1)
PY
  then
    exit 1
  fi
  echo "$sha -> $parent  $(git log -1 --format=%s "$sha")"
  prev=$sha
done

echo "final server commit: $parent"
echo "$parent"
