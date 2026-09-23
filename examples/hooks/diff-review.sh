#!/bin/sh
# Asks a small model to review the uncommitted diff; fails on a concrete problem.
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo '{"status":"pass"}'; exit 0; }
diff=$(git diff HEAD 2>/dev/null | head -c 60000)
[ -z "$diff" ] && { echo '{"status":"pass"}'; exit 0; }
system='You review a code diff for concrete defects only: bugs, leftover debug code, secrets, broken error handling. Reply with JSON only: {"status":"pass"} or {"status":"fail","message":"<one sentence naming file and problem>","fix":"<one sentence>"}.'
reply=$(printf '%s' "$diff" | "$ASHKELON_BIN" model fast --system "$system") || { echo '{"status":"pass"}'; exit 0; }
printf '%s' "$reply" | sed -n '/{/,/}/p' | jq -c 'select(.status=="pass" or .status=="fail")' 2>/dev/null | head -1 | grep . || echo '{"status":"pass"}'
