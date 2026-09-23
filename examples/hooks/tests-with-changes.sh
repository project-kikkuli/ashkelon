#!/bin/sh
# Fails when source files changed but no test file did.
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo '{"status":"pass"}'; exit 0; }
changed=$( { git diff --name-only HEAD; git ls-files --others --exclude-standard; } 2>/dev/null | sort -u)
[ -z "$changed" ] && { echo '{"status":"pass"}'; exit 0; }
src=$(printf '%s\n' "$changed" | grep -Ev '(^|/)(tests?|spec|__tests__)/|_test\.|\.test\.|_spec\.|\.spec\.|\.md$|\.toml$|\.json$|\.lock$' || true)
tests=$(printf '%s\n' "$changed" | grep -E '(^|/)(tests?|spec|__tests__)/|_test\.|\.test\.|_spec\.|\.spec\.' || true)
if [ -n "$src" ] && [ -z "$tests" ]; then
  jq -n --arg s "$src" '{status:"fail", message:("Source changed with no test changes: " + ($s|split("\n")|join(", "))), fix:"Add or update tests covering these changes, or say why none are needed."}'
else
  echo '{"status":"pass"}'
fi
