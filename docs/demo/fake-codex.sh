#!/usr/bin/env bash
# Looks like a short coding-agent session in the terminal. Not the real Codex CLI.
# Fires the real `unwreck hook codex` payloads (docs fixtures, cwd/prompt filled in).
set -euo pipefail

prompt=${1:-fix the failing tests}
cwd=$(pwd)
root=${UNWRECK_DEMO_ROOT:?UNWRECK_DEMO_ROOT must point at the unwreck repo}
fx=$root/code/crates/unwreck-hooks/fixtures/docs
payload=$(mktemp "${TMPDIR:-/tmp}/uw-hook.XXXXXX")
trap 'rm -f "$payload"' EXIT

wait_s() {
  python3 -c "import time; time.sleep(float('$1'))"
}

# TUI recordings set FAKE_CODEX_SLEEP to keep the hidden setup short.
if [[ -n "${FAKE_CODEX_SLEEP:-}" ]]; then
  think=$FAKE_CODEX_SLEEP
  running=$FAKE_CODEX_SLEEP
  editing=$FAKE_CODEX_SLEEP
  done_wait=$FAKE_CODEX_SLEEP
  after_start=$FAKE_CODEX_SLEEP
  between=$FAKE_CODEX_SLEEP
else
  think=1.2
  running=0.8
  editing=0.8
  done_wait=0.5
  after_start=2
  between=1
fi

hook() {
  python3 - "$fx/$1" "$cwd" "$prompt" "$payload" <<'PY'
import json, sys
src, cwd, prompt, dest = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
with open(src, encoding="utf-8") as f:
    body = json.load(f)
body["cwd"] = cwd
if body.get("hook_event_name") == "UserPromptSubmit":
    body["prompt"] = prompt
tool = dict(body.get("tool_input") or {})
if "command" in tool:
    tool["command"] = "rm -rf src/gen .env"
if "file_path" in tool:
    tool["file_path"] = "src/main.rs"
if tool:
    body["tool_input"] = tool
with open(dest, "w", encoding="utf-8") as f:
    json.dump(body, f)
PY
  # Keep the agent TTY clean: hooks must not inherit this stdout.
  unwreck hook codex <"$payload" >/dev/null 2>&1
}

echo "> $prompt"
echo "- Thinking..."
wait_s "$think"
hook codex-UserPromptSubmit.json
wait_s "$after_start"
echo "- Running rm -rf src/gen .env"
hook codex-PreToolUse-Bash.json
rm -rf src/gen .env
wait_s "$running"
wait_s "$between"
echo "- Editing src/main.rs"
hook codex-PreToolUse-apply_patch.json
echo 'fn main() {}' > src/main.rs
wait_s "$editing"
wait_s "$between"
echo "- Done"
hook codex-Stop.json
wait_s "$done_wait"
