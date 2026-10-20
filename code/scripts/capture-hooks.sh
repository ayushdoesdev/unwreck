#!/usr/bin/env bash
# Record a real agent hook payload, then run `unwreck hook`.
# Point the agent's hook command at this script (see --help).
set -euo pipefail
usage() {
  cat <<'EOF'
Usage: capture-hooks.sh [--agent claude|codex|gemini] [--out DIR]

Reads hook JSON on stdin, writes it under DIR (default:
  crates/unwreck-hooks/fixtures/captured/), then execs `unwreck hook AGENT`.
Label files yourself after capture; docs/ fixtures are from vendor docs, not live.
EOF
}
AGENT=claude
OUT="${UNWRECK_CAPTURE_DIR:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --agent) AGENT=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
if [ -z "$OUT" ]; then
  HERE=$(cd "$(dirname "$0")/.." && pwd)
  OUT="$HERE/crates/unwreck-hooks/fixtures/captured"
fi
mkdir -p "$OUT"
RAW=$(mktemp)
trap 'rm -f "$RAW"' EXIT
cat > "$RAW"
TS=$(date +%Y%m%d-%H%M%S)
EVENT=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("hook_event_name","unknown"))' "$RAW" 2>/dev/null || echo unknown)
DEST="$OUT/${AGENT}-${EVENT}-${TS}.json"
cp "$RAW" "$DEST"
echo "unwreck: captured $DEST" >&2
exec unwreck hook "$AGENT" < "$RAW"
