#!/usr/bin/env bash
# Synthetic benchmark: 10,000 files / ~40 MB in 100 dirs (4,000 bytes each, unique
# first line, compressible body). Usage: scripts/bench.sh <path-to-unwreck-binary>
# BENCH_AGE=1 backdates every file's mtime by one hour (like a real checkout), so the
# stat cache may trust them; without it, all files are seconds old and count as "racy".
set -euo pipefail
BIN=$(realpath "$1")
W=$(mktemp -d /tmp/uwbench.XXXX)
python3 - "$W/repo" <<'PY'
import os, sys, random
root = sys.argv[1]; random.seed(42)
words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".split()
for d in range(100):
    os.makedirs(f"{root}/src/m{d:03}", exist_ok=True)
    for f in range(100):
        i = d * 100 + f
        head = f"// file {i} {random.random()}\n"
        body = " ".join(random.choice(words) for _ in range(60)) + "\n"
        data = (head + body * 20)[:4000]
        p = f"{root}/src/m{d:03}/f{f:03}.rs"
        open(p, "w").write(data)
        if os.environ.get("BENCH_AGE") == "1":
            t = __import__("time").time() - 3600
            os.utime(p, (t, t))
PY
export UNWRECK_DIR="$W/store"
RUNS=${UNCHANGED_RUNS:-20}
cd "$W/repo"
echo "tree: $(find . -type f | wc -l) files, $(du -sb . 2>/dev/null | cut -f1 || du -sk . | awk '{print $1*1024}') bytes"
echo "BENCH_AGE=${BENCH_AGE:-0} UNCHANGED_RUNS=$RUNS"
python3 - "$BIN" "$RUNS" "$W/repo" <<'PY'
import json, os, subprocess, sys, time
bin, n, repo = sys.argv[1], int(sys.argv[2]), sys.argv[3]

def pct(xs, p):
    ys = sorted(xs)
    k = (len(ys) - 1) * p / 100.0
    f = int(k)
    c = min(f + 1, len(ys) - 1)
    return ys[f] + (ys[c] - ys[f]) * (k - f)

def ms_snap():
    t = time.perf_counter()
    subprocess.check_call([bin, "snap", "-m", "first"], stdout=subprocess.DEVNULL)
    return (time.perf_counter() - t) * 1000.0

def ms_hook():
    payload = json.dumps({
        "hook_event_name": "PreToolUse",
        "cwd": repo,
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
    }).encode()
    t = time.perf_counter()
    r = subprocess.run([bin, "hook", "claude"], input=payload, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.returncode != 0 or r.stdout:
        raise SystemExit(f"hook failed: {r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}")
    return (time.perf_counter() - t) * 1000.0

print(f"first snapshot ms: {ms_snap():.1f}")
times = [ms_hook() for _ in range(n)]
for i, t in enumerate(times, 1):
    print(f"unchanged re-snapshot ms (run {i}): {t:.1f}")
print(f"unchanged p50 ms: {pct(times, 50):.1f}")
print(f"unchanged p95 ms: {pct(times, 95):.1f}")
PY
echo "store apparent bytes: $(du -sb --apparent-size "$UNWRECK_DIR" 2>/dev/null | cut -f1 || du -sk "$UNWRECK_DIR" | awk '{print $1*1024}')"
echo "store disk usage (du -sk): $(du -sk "$UNWRECK_DIR" | cut -f1) KiB"
echo "store files: $(find "$UNWRECK_DIR" -type f | wc -l)"
rm -rf "$W"
