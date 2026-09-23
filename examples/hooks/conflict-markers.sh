#!/bin/sh
# Fails when the working tree contains unresolved merge-conflict markers.
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo '{"status":"pass"}'; exit 0; }
files=$(git diff --name-only HEAD 2>/dev/null | while read -r f; do
  [ -f "$f" ] && grep -lE '^(<<<<<<<|>>>>>>>) ' "$f"
done)
if [ -n "$files" ]; then
  jq -n --arg f "$files" '{status:"fail", message:("Unresolved conflict markers in: " + ($f|split("\n")|join(", "))), fix:"Resolve the conflicts in those files."}'
else
  echo '{"status":"pass"}'
fi
