#!/usr/bin/env bash
# Smoke-test every web page for 200s and missing-template/rendering errors.
# Uses the demo repos from tests/seed_demo.py; run that first.
set -u
EP="${1:-http://127.0.0.1:8080}"
TOKEN="${2:?token required}"
PASSWORD="${3:-adminpass123}"
COOKIE=$(mktemp)
pass=0; fail=0
MODEL="admin/Qwen2.5-Demo-0.5B-Instruct"
check() { # path expected_code
  local path="$1" want="${2:-200}"
  local out code body
  out=$(curl -s -b "$COOKIE" -c "$COOKIE" -w '\n%{http_code}' "$EP$path")
  code=$(echo "$out" | tail -1)
  body=$(echo "$out" | sed '$d')
  if [ "$code" = "$want" ] && ! echo "$body" | grep -qiE "internal error|jinja|panic|template.*not found|no such template|&lt;div"; then
    # relative hrefs would resolve to a bogus host like http://tree/main
    if echo "$body" | grep -qE 'href="//[a-z]'; then
      echo "  FAIL $code $path (relative href: $(echo "$body" | grep -oE 'href="//[a-z][^"]*"' | head -1))"; fail=$((fail+1)); return
    fi
    echo "  ok   $code $path"; pass=$((pass+1))
  else
    echo "  FAIL $code $path"; echo "$body" | grep -iE "error|panic" | head -3; fail=$((fail+1))
  fi
}
echo "Web UI smoke test against $EP"
check "/"
check "/models"
check "/datasets"
check "/spaces"
check "/docs"
check "/login"
check "/join"
check "/$MODEL"
check "/$MODEL/tree/main"
check "/$MODEL/blob/main/config.json"
check "/$MODEL/commits/main"
check "/$MODEL/community"
check "/datasets/admin/alpaca-demo"
check "/spaces/admin/vision-demo"
check "/$MODEL/commit/$(curl -s "$EP/api/models/$MODEL/commits/main" | grep -oE '"id":"[a-f0-9]+"' | head -1 | cut -d'"' -f4)"
check "/settings/profile" 303
check "/admin" 303
check "/nonexistent-page" 404

# log in through the form and re-check authenticated pages
curl -s -c "$COOKIE" -b "$COOKIE" -o /dev/null -X POST "$EP/login" -d "username=admin&password=$PASSWORD"
check "/settings/profile"
check "/settings/tokens"
check "/$MODEL/settings"
check "/$MODEL/upload/main"
check "/new"
check "/new-space"
check "/organizations"
check "/organizations/new"
check "/admin"
check "/admin/users"
check "/admin/repos"
check "/admin/orgs"
check "/admin/server"
check "/$MODEL/edit/main/config.json"

# form endpoints that are easy to leave unwired
code=$(curl -s -b "$COOKIE" -o /dev/null -w '%{http_code}' -X POST "$EP/admin/follow")
[ "$code" = "303" ] && { echo "  ok   303 POST /admin/follow"; pass=$((pass+1)); } || { echo "  FAIL $code POST /admin/follow"; fail=$((fail+1)); }
code=$(curl -s -b "$COOKIE" -o /dev/null -w '%{http_code}' -X POST "$EP/$MODEL/like")
[ "$code" = "303" ] && { echo "  ok   303 POST /$MODEL/like"; pass=$((pass+1)); } || { echo "  FAIL $code POST /$MODEL/like"; fail=$((fail+1)); }

echo
echo "$pass passed, $fail failed"
rm -f "$COOKIE"
[ "$fail" -eq 0 ]
