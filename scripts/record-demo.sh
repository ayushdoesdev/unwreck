#!/usr/bin/env bash
# Rebuild the launch GIFs/MP4 with VHS: https://github.com/charmbracelet/vhs
# Usage: scripts/record-demo.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if ! command -v vhs >/dev/null 2>&1; then
  echo "vhs is not installed. On macOS: brew install vhs" >&2
  echo "Needs ttyd and ffmpeg on PATH as well." >&2
  exit 1
fi

echo "building release unwreck…"
cargo build --release --manifest-path "$root/code/Cargo.toml" -p unwreck
export PATH="$root/code/target/release:$PATH"
export UNWRECK_DEMO_ROOT="$root"
# Keep the recording shell from picking up a fancy prompt / rustup noise.
export PS1='$ '
export HOME="${HOME:-/tmp}"
export LANG="${LANG:-en_US.UTF-8}"
export LC_ALL="${LC_ALL:-en_US.UTF-8}"

mkdir -p "$root/docs/demo"
chmod +x "$root/docs/demo/fake-codex.sh" "$root/docs/demo/setup-repo.sh"
echo "recording demo.tape…"
vhs "$root/docs/demo/demo.tape"
echo "recording tui.tape…"
vhs "$root/docs/demo/tui.tape"

python3 - "$root/docs/demo/demo.gif" "$root/docs/demo/tui.gif" <<'PY'
import os, sys
ok = True
limits = {sys.argv[1]: 1.5 * 1024 * 1024, sys.argv[2]: 3 * 1024 * 1024}
for p, limit in limits.items():
    n = os.path.getsize(p)
    mb = n / (1024 * 1024)
    print(f"{os.path.relpath(p)}  {mb:.2f} MiB")
    if n > limit:
        print(f"warning: {p} is over {limit / (1024 * 1024):.1f} MiB; lower Set Framerate or shorten Sleep", file=sys.stderr)
        ok = False
if not ok:
    sys.exit(1)
PY

ls -lh "$root/docs/demo/demo.gif" "$root/docs/demo/demo.mp4" "$root/docs/demo/tui.gif"
echo "ok"
