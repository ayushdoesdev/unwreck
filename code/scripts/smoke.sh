#!/usr/bin/env bash
# End-to-end smoke test: Claude-style hooks -> rm -rf -> undo -> redo -> conflict -> gc.
# Usage: scripts/smoke.sh <path-to-unwreck-binary>. Exits non-zero on any failed check.
set -euo pipefail
B=$(realpath "$1"); T=$(mktemp -d /tmp/uwsmoke.XXXX); trap 'rm -rf "$T"' EXIT
export UNWRECK_DIR=$T/store; R=$T/repo
fail() { echo "FAIL: $*"; exit 1; }
mkdir -p "$R/src/gen" "$R/logs"; cd "$R"; git init -q
echo 'fn a(){}' > src/lib.rs; echo gen > src/gen/x.rs; echo SECRET=1 > .env; echo '.env' > .gitignore
git add -A; git -c user.email=a@b -c user.name=a commit -qm init
hook() { out=$(echo "$1" | "$B" hook claude); [ -z "$out" ] || fail "hook wrote to stdout"; }
hook '{"session_id":"s1","hook_event_name":"UserPromptSubmit","cwd":"'$R'","prompt":"clean up generated code","prompt_id":"p1"}'
hook '{"session_id":"s1","hook_event_name":"PreToolUse","cwd":"'$R'","tool_name":"Bash","tool_input":{"command":"rm -rf src/gen .env logs"}}'
rm -rf src/gen .env logs; echo 'fn a(){ broken }' > src/lib.rs
hook '{"session_id":"s1","hook_event_name":"Stop","cwd":"'$R'"}'
"$B" log
echo "--- diff"
"$B" diff | grep -q '.env' || fail "diff no-args missed last turn-start vs disk"
echo "--- undo"; "$B" undo
[ "$(cat .env)" = SECRET=1 ] && [ -f src/gen/x.rs ] && [ "$(cat src/lib.rs)" = 'fn a(){}' ] || fail "undo did not restore files"
[ -d logs ] || fail "empty dir logs/ not recreated"
echo "--- redo"; "$B" redo
[ ! -e src/gen ] || fail "src/gen still exists after redo"
[ ! -e logs ] || fail "logs/ (empty, not in target) still exists after redo"
[ ! -e .env ] && [ "$(cat src/lib.rs)" = 'fn a(){ broken }' ] || fail "redo did not return to agent state"
echo "--- undo again, then redo with a user edit (conflict)"; "$B" undo >/dev/null
echo 'my edit' > src/lib.rs; "$B" redo
[ "$(cat src/lib.rs)" = 'my edit' ] || fail "conflicting user edit was overwritten"
[ ! -e src/gen ] || fail "src/gen not pruned in conflict case"
echo "--- user file in an agent-created dir survives"; "$B" undo >/dev/null; "$B" snap -m mid >/dev/null
echo mine > src/gen/mine.txt; "$B" redo
[ -f src/gen/mine.txt ] || fail "user file removed"
echo "--- gc"; "$B" gc --keep 3
ls "$UNWRECK_DIR"/projects/*/packs
[ -z "$(ls -A "$R" | grep -i unwreck || true)" ] || fail "store inside repo"
echo "--- hooks write"
"$B" hooks --write --agent claude --dry-run | grep -q '^+++' || fail "dry-run missing diff"
"$B" hooks --write --agent claude
"$B" hooks --write --agent claude | grep -q 'already up to date' || fail "second write was not idempotent"
"$B" doctor --json | grep -q '"ok": true' || fail "doctor --json"
"$B" hooks --remove --agent claude
[ ! -f "$R/.claude/settings.json" ] || fail "remove left settings.json"
echo "SMOKE OK"
