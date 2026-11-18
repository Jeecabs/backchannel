#!/usr/bin/env bash
# End-to-end check against a running relay (default: `wrangler dev` on :8787 with
# BOOTSTRAP_CODE=dev-bootstrap-code in crates/relay/.dev.vars, fresh state).
set -euo pipefail
B=${1:-http://127.0.0.1:8787}
BOOT=${BOOTSTRAP_CODE:-dev-bootstrap-code}
j() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
call() {
  local args="${4:-}"; [ -z "$args" ] && args="{}"
  curl -s -X POST "$B/v1/call/$2" -H "authorization: Bearer $1" -H "x-backchannel-agent: ${3:-cli}" -H 'content-type: application/json' -d "$args" | j 'd.get("text") or d'
}
expect() { grep -q -- "$1" <<<"$2" || { echo "FAIL: expected '$1' in:"; echo "$2"; exit 1; }; echo "ok: $1"; }

curl -s --retry 60 --retry-connrefused --retry-delay 2 "$B/.well-known/backchannel" >/dev/null

ADMIN=$(curl -sf -X POST "$B/v1/signup" -H 'content-type: application/json' -d "{\"name\":\"lachlan\",\"display_name\":\"Lachlan\",\"invite\":\"$BOOT\"}" | j 'd["token"]')
INV=$(call "$ADMIN" create_invite cli '{"uses":2}')
CODE=$(grep -oE 'CHAN-[0-9A-Z]{4}-[0-9A-Z]{4}' <<<"$INV" | head -1)
expect "CHAN-" "$INV"
BOB=$(curl -sf -X POST "$B/v1/signup" -H 'content-type: application/json' -d "{\"name\":\"bob\",\"invite\":\"$CODE\"}" | j 'd["token"]')

# Random people can't just join: no invite means a request the admin must approve.
REQ=$(curl -sf -X POST "$B/v1/signup" -H 'content-type: application/json' -d '{"name":"carol","note":"friend of bob"}')
expect pending "$REQ"
RID=$(j 'd["request_id"]' <<<"$REQ"); RS=$(j 'd["secret"]' <<<"$REQ")
expect "carol" "$(call "$ADMIN" requests)"
call "$ADMIN" approve cli '{"name":"carol"}' >/dev/null
CAROL=$(curl -sf "$B/v1/signup/status?id=$RID&secret=$RS" | j 'd["token"]')
expect "collected" "$(curl -sf "$B/v1/signup/status?id=$RID&secret=$RS")"

# Members can't invite.
expect "only admins" "$(curl -s -X POST "$B/v1/call/create_invite" -H "authorization: Bearer $BOB" -H 'content-type: application/json' -d '{}')"

# Bob's agent asks smoko and mentions lachlan; lachlan's new agent sees it.
POSTED=$(call "$BOB" post reviewer '{"board":"smoko","kind":"ask","body":"anyone free to look at PR 7? @lachlan"}')
ID=$(grep -oE 'id [0-9A-Z]{26}' <<<"$POSTED" | cut -c4-)
IN=$(call "$ADMIN" inbox frontend)
expect "PR 7" "$IN"; expect "mentions you" "$IN"; expect "SECURITY NOTE" "$IN"; expect "END OF BACKCHANNEL MESSAGES" "$IN"

# Claim, conflict, handled: global state everyone sees.
ARGS_ID='{"id":"'"$ID"'"}'
ARGS_DONE='{"id":"'"$ID"'","note":"looks good"}'
expect "claimed by lachlan/frontend" "$(call "$ADMIN" claim frontend "$ARGS_ID")"
expect "claimed by lachlan/frontend" "$(curl -s -X POST "$B/v1/call/claim" -H "authorization: Bearer $CAROL" -H 'content-type: application/json' -d "$ARGS_ID")"
expect "handled by lachlan/frontend" "$(call "$ADMIN" handled frontend "$ARGS_DONE")"
expect "handled on your ask" "$(call "$BOB" inbox reviewer)"

# Direct messages, to a person and to one agent.
call "$CAROL" post cli '{"to":"bob/reviewer","body":"psst, private"}' >/dev/null
expect "psst" "$(call "$BOB" inbox reviewer)"
expect "Nothing new" "$(call "$ADMIN" inbox frontend)"

# Long-poll wait wakes up when a message arrives.
( sleep 2; call "$CAROL" post cli '{"to":"lachlan","body":"you there?"}' >/dev/null ) &
expect "you there" "$(call "$ADMIN" wait frontend '{"timeout_seconds":20}')"
wait

# Webhooks post as a named source.
call "$ADMIN" create_board cli '{"name":"ops/deploys","description":"Deploy notices"}' >/dev/null
HOOK=$(call "$ADMIN" create_hook cli '{"source":"deploys","board":"ops/deploys"}')
URL=$(grep -oE 'https?://[^ ]+/hooks/[^ ]+' <<<"$HOOK" | head -1 | sed -E "s#^https?://[^/]+#$B#")
expect '"ok":true' "$(curl -s -X POST "$URL" -H 'content-type: application/json' -d '{"text":"web deployed to prod","status":"ok","sha":"abc123"}')"
call "$BOB" subscribe reviewer '{"pattern":"ops/**"}' >/dev/null
curl -s -X POST "$URL" -d 'plain text alert: disk 91%' >/dev/null
expect "deploys/webhook" "$(call "$BOB" inbox reviewer)"

# Hosted MCP over Streamable HTTP.
H=$(mktemp)
INIT=$(curl -s -D "$H" -X POST "$B/mcp" -H "authorization: Bearer $BOB" -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"1"}}}')
expect '"protocolVersion":"2025-06-18"' "$INIT"
SID=$(grep -i '^mcp-session-id' "$H" | cut -d' ' -f2 | tr -d '\r')
mcp() { curl -s -X POST "$B/mcp" -H "authorization: Bearer $BOB" -H "mcp-session-id: $SID" -H 'content-type: application/json' -d "$1"; }
expect '"name":"post"' "$(mcp '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')"
expect 'You are bob/qa' "$(mcp '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"whoami","arguments":{"label":"qa"}}}')"
expect 'bob/qa' "$(mcp '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"directory","arguments":{}}}')"

# Anonymous MCP only offers join.
ANON=$(curl -s -X POST "$B/mcp" -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}')
expect '"name":"join"' "$ANON"
# Token in the URL works for clients that only take a URL.
expect '"name":"post"' "$(curl -s -X POST "$B/mcp/$BOB" -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}')"

# Viewer is admin-only.
expect '"posts"' "$(curl -s "$B/v1/view/feed" -H "authorization: Bearer $ADMIN")"
expect 'for admins' "$(curl -s "$B/v1/view/state" -H "authorization: Bearer $BOB")"

echo
echo "All relay checks passed."
echo "ADMIN_TOKEN=$ADMIN"
