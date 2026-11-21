#!/usr/bin/env bash
# Type-checks the Claude Code mod (mods/claude-code) against Claude Code's
# plugin API, and validates the plugin and marketplace manifests when the
# `claude` CLI is installed. Needs bun (for `bunx tsc`).
#
# The API's declarations belong to Claude Code and are not kept in this
# repository. Claude Code writes them beside a mod it loads from a folder you
# own, at mods/claude-code/.claude-plugin/types/claude-code/index.d.ts (run
# `claude --plugin-dir mods/claude-code` once). When they aren't there, this
# script looks for a copy Claude Code already has on this machine, then
# downloads the version pinned below. Set CLAUDE_CODE_TYPES to a file to use
# that one instead.
set -euo pipefail
cd "$(dirname "$0")/.."
mod=mods/claude-code
types="$mod/.claude-plugin/types/claude-code/index.d.ts"

# The commit of github.com/anthropics/claude-code whose declarations the mod was
# last checked against. The API is early access and changes; move this forward
# when the mod is updated for a newer Claude Code.
TYPES_REF=${CLAUDE_CODE_TYPES_REF:-08859bd09c737c8531b1a3a6c7189274b38bf970}
local_copy="$HOME/.claude/plugins/marketplaces/claude-code-plugins/mods/types/claude-code.d.ts"

mkdir -p "$(dirname "$types")"
if [ -n "${CLAUDE_CODE_TYPES:-}" ]; then
  cp "$CLAUDE_CODE_TYPES" "$types"
elif [ -s "$types" ]; then
  :
elif [ -s "$local_copy" ]; then
  cp "$local_copy" "$types"
else
  curl -fsSL "https://raw.githubusercontent.com/anthropics/claude-code/$TYPES_REF/mods/types/claude-code.d.ts" -o "$types"
fi
echo "Checking $mod against $(grep -c '' "$types") lines of declarations in $types"

bunx --package typescript@5.9.3 tsc --noEmit -p "$mod"
echo "ok: $mod type-checks"

if command -v claude >/dev/null 2>&1; then
  claude plugin validate . >/dev/null && echo "ok: claude plugin validate . (the marketplace)"
  claude plugin validate "$mod" >/dev/null && echo "ok: claude plugin validate $mod"
else
  echo "skipped: claude plugin validate (no claude CLI here)"
fi
