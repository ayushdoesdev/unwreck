# Changelog

## 0.1.0 — 2026-10-18

First public release.

- Snapshot working trees at agent turns (Claude Code, Codex, Gemini CLI hooks) and via `unwreck run` / `watch` for any other CLI.
- `unwreck undo` / `restore` / `redo` with conflict-aware restore (never silently overwrite user edits; never touch `.git`).
- Atomic file replace on restore; store outside the repo with Unix 0700/0600.
- `unwreck hooks --write` / `--remove` / `--dry-run` (merge, backup, idempotent). Codex `PreToolUse` matcher is `"*"` so shell tools snapshot; re-install upgrades an older matcher.
- `unwreck diff` with no args compares the last turn-start snapshot to the working tree (`diff N`, `diff N M` still work).
- `unwreck doctor` (✓/!/✗, `--json`, exit 1 only on fail).
- TUI timeline with file list and unified diff (`similar`).
- `gc` retention via `~/.config/unwreck/config.toml` and `.unwreck.toml`; packs rewritten only when more than 30% dead.
- Content-addressed BLAKE3 + zstd packs, no-op reuse, parallel scan.

Measured on this box (release, `scripts/bench.sh`, `BENCH_AGE=1`, 20 runs): unchanged 10k-file hook snapshot p50 27.0 ms / p95 28.1 ms.

Release binaries for 0.1.0: x86_64 Linux musl, x86_64/aarch64 macOS, x86_64 Windows. **aarch64 Linux is planned for 0.1.1.**
