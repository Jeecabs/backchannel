#!/usr/bin/env bash
# End-to-end check against a running relay (default: `wrangler dev` on :8787, fresh state).
set -euo pipefail
B=${1:-http://127.0.0.1:8787}
j() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
call() {
  local args="${4:-}"; [ -z "$args" ] && args="{}"
  curl -s -X POST "$B/v1/call/$2" -H "authorization: Bearer $1" -H "x-backchannel-agent: ${3:-cli}" -H 'content-type: application/json' -d "$args" | j 'd.get("text") or d'
}
expect() { grep -q -- "$1" <<<"$2" || { echo "FAIL: expected '$1' in:"; echo "$2"; exit 1; }; echo "ok: $1"; }

curl -s --retry 60 --retry-connrefused --retry-delay 2 "$B/.well-known/backchannel" >/dev/null

post() { curl -s -X POST "$B$1" -H 'content-type: application/json' -d "$2"; }
# Whoever claims an org first is its admin. A second claim on the same name fails.
ADMIN=$(post /v1/orgs '{"org":"acme","name":"lachlan","display_name":"Lachlan"}' | j 'd["token"]')
expect "is taken" "$(post /v1/orgs '{"org":"acme","name":"mallory"}')"
INV=$(call "$ADMIN" create_invite cli '{"uses":2}')
CODE=$(grep -oE 'CHAN-[0-9A-Z]{4}-[0-9A-Z]{4}' <<<"$INV" | head -1)
expect "CHAN-" "$INV"
expect '"org":"acme"' "$(curl -s "$B/v1/invite/$CODE")"
BOB=$(curl -sf -X POST "$B/v1/signup" -H 'content-type: application/json' -d "{\"name\":\"bob\",\"invite\":\"$CODE\"}" | j 'd["token"]')

# Random people can't just join: no invite means a request the admin must approve.
expect "which org" "$(post /v1/signup '{"name":"carol"}')"
expect "no org called" "$(post /v1/signup '{"name":"carol","org":"nope"}')"
REQ=$(post /v1/signup '{"name":"carol","org":"acme","note":"friend of bob"}')
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

# The install script points at this relay, and the MCP endpoint is gone.
expect "/dl/backchannel-" "$(curl -s "$B/install.sh")"
expect '404' "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$B/mcp" -d '{}')"

# ---- Org isolation: a second org on the same database sees none of the above. ----
G=$(post /v1/orgs '{"org":"globex","name":"lachlan"}' | j 'd["token"]')   # same person name, different org
GINV=$(call "$G" create_invite cli '{}'); GCODE=$(grep -oE 'CHAN-[0-9A-Z]{4}-[0-9A-Z]{4}' <<<"$GINV" | head -1)
expect '"org":"globex"' "$(curl -s "$B/v1/invite/$GCODE")"
GBOB=$(post /v1/signup "{\"name\":\"bob\",\"invite\":\"$GCODE\"}" | j 'd["token"]')   # bob exists in acme too
no() { if grep -q -- "$1" <<<"$2"; then echo "FAIL: '$1' leaked into:"; echo "$2"; exit 1; fi; echo "ok: no $1"; }
GDIR=$(call "$G" directory); no carol "$GDIR"; no deploys "$GDIR"
GREAD=$(call "$G" read cli '{"board":"smoko"}'); no "PR 7" "$GREAD"
no "ops/deploys" "$(call "$G" boards)"
no "PR 7" "$(call "$G" inbox frontend)"; no "you there" "$(call "$G" inbox frontend)"
GFEED=$(curl -s "$B/v1/view/feed" -H "authorization: Bearer $G"); no "PR 7" "$GFEED"; no "psst" "$GFEED"; no "disk 91" "$GFEED"
GSTATE=$(curl -s "$B/v1/view/state" -H "authorization: Bearer $G"); no carol "$GSTATE"; no "$CODE" "$GSTATE"; expect '"org":"globex"' "$GSTATE"
# Globex can't touch acme's post, requests, members, invites or hooks.
no "claimed by lachlan/cli" "$(curl -s -X POST "$B/v1/call/claim" -H "authorization: Bearer $G" -H 'content-type: application/json' -d "$ARGS_ID")"
no "PR 7" "$(curl -s -X POST "$B/v1/call/read" -H "authorization: Bearer $G" -H 'content-type: application/json' -d "{\"thread\":\"$ID\"}")"
expect "Nobody is waiting" "$(call "$G" requests)"
no "deploys" "$(call "$G" hooks)"
call "$G" remove_member cli '{"name":"bob"}' >/dev/null   # removes globex's bob only
expect "bob/reviewer" "$(call "$BOB" whoami reviewer)"   # acme's bob still works
expect "unknown\|missing\|unauthorized" "$(curl -s -X POST "$B/v1/call/whoami" -H "authorization: Bearer $GBOB" -H 'content-type: application/json' -d '{}')"
# A post in globex's smoko starts its own numbering and stays out of acme.
expect "seq 1" "$(call "$G" post cli '{"board":"smoko","body":"globex only"}')"
no "globex only" "$(call "$ADMIN" read cli '{"board":"smoko"}')"
no "globex" "$(curl -s "$B/v1/view/feed" -H "authorization: Bearer $ADMIN")"
expect '"org":"acme"' "$(curl -s "$B/v1/view/state" -H "authorization: Bearer $ADMIN")"

# Viewer is admin-only.
expect '"posts"' "$(curl -s "$B/v1/view/feed" -H "authorization: Bearer $ADMIN")"
expect 'for admins' "$(curl -s "$B/v1/view/state" -H "authorization: Bearer $BOB")"

echo
echo "All relay checks passed."
echo "ADMIN_TOKEN=$ADMIN"
