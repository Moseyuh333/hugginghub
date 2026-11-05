#!/usr/bin/env bash
# Git smart-HTTP + git-lfs end-to-end test (idempotent: uses a fresh repo each run).
# Usage: tests/e2e_git.sh http://host:port hf_token
set -u
EP="${1:-http://127.0.0.1:8080}"
TOKEN="${2:?token required}"
HOST="${EP#*://}"
STAMP=$(date +%s)
REPO="admin/git-test-$STAMP"
URL_AUTH="http://admin:$TOKEN@$HOST/$REPO"
URL_ANON="$EP/$REPO"
DIR=$(mktemp -d)
export GIT_TERMINAL_PROMPT=0
pass=0; fail=0
ok()  { echo "  ok   $1"; pass=$((pass+1)); }
bad() { echo "  FAIL $1"; fail=$((fail+1)); }

echo "Testing git + git-lfs against $EP (repo $REPO)"
cd "$DIR" || exit 1

curl -s -X POST "$EP/api/repos/create" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d "{\"name\":\"$REPO\",\"type\":\"model\"}" >/dev/null

git clone "$URL_AUTH" clone >/dev/null 2>&1 && ok "clone (authenticated)" || bad "clone (authenticated)"
cd clone || exit 1
git config user.email dev@example.com
git config user.name dev
git lfs install --local >/dev/null 2>&1

echo "hello git" > hello.txt
git add hello.txt && git commit -qm "add hello" && git push -q origin HEAD:main \
  && ok "push text file" || bad "push text file"

head -c 2000000 /dev/urandom > model.bin
git lfs track "*.bin" >/dev/null 2>&1
git add .gitattributes model.bin && git commit -qm "add lfs blob" && git push -q origin HEAD:main \
  && ok "push LFS file" || bad "push LFS file"

cd "$DIR" || exit 1
git clone "$URL_AUTH" clone2 >/dev/null 2>&1 \
  && { cmp -s clone/model.bin clone2/model.bin && ok "git-lfs smudge on clone" || bad "git-lfs smudge on clone"; } \
  || bad "second clone"

cd "$DIR/clone" || exit 1
git show HEAD:model.bin | head -c 40 | grep -q "version https://git-lfs" && ok "pointer stored in git" || bad "pointer stored in git"

cd "$DIR" || exit 1
git clone "$URL_ANON" clone3 >/dev/null 2>&1 && ok "anonymous clone" || bad "anonymous clone"
cd "$DIR/clone" || exit 1
echo x >> hello.txt && git commit -qam "anon push attempt"
git push -q "$URL_ANON" HEAD:main >/dev/null 2>&1 && bad "anonymous push rejected" || ok "anonymous push rejected"

echo "pr change" > pr.txt && git add pr.txt && git commit -qm "pr change" && git push -q origin HEAD:refs/pr/1 \
  && ok "push to refs/pr/1" || bad "push to refs/pr/1"

code=$(curl -s -o /dev/null -w '%{http_code}' "$EP/api/models/$REPO/commits/main")
[ "$code" = "200" ] && ok "API lists pushed commits" || bad "API lists pushed commits ($code)"

curl -s "$EP/$REPO/resolve/main/hello.txt" | grep -q "hello git" && ok "resolve pushed file" || bad "resolve pushed file"

echo
echo "$pass passed, $fail failed"
rm -rf "$DIR"
[ "$fail" -eq 0 ]
