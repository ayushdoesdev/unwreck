# Contributing to unwreck

The Cargo workspace lives in `code/`. MSRV is **1.90**.

## Setup

```sh
cd code
cargo test --workspace
scripts/smoke.sh target/debug/unwreck
```

## Rules that must not regress

- Restore never silently overwrites a file the user changed after the last snapshot (`Conflict` unless `--force`). `--force` is always recoverable via `unwreck redo`.
- Restore never touches `.git`.
- `unwreck hook` always exits 0 and writes nothing to stdout (errors go to stderr).
- The snapshot store stays outside the worktree (`UNWRECK_DIR` or the platform data dir).
- Existing manifests stay readable (no breaking snapshot format without a version bump).
- Unchanged 10k-file hook snapshot: p50 ≤ 30 ms (`scripts/bench.sh`, `BENCH_AGE=1`, release).

## After every change

From `code/`:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
scripts/smoke.sh target/debug/unwreck
# performance work: BENCH_AGE=1 scripts/bench.sh target/release/unwreck
```

Libraries use `thiserror`; the binary uses `anyhow`. Public items need rustdoc (`#![warn(missing_docs)]` on library crates).

## Hooks

Vendor payload JSON under `code/crates/unwreck-hooks/fixtures/docs/` is **from docs, not captured**. To record live payloads, point an agent hook at `code/scripts/capture-hooks.sh`.

## Config

- User: `~/.config/unwreck/config.toml` (or `UNWRECK_CONFIG`)
- Repo: `.unwreck.toml`

```toml
[gc]
keep_turns = 500          # newest N snapshots
keep_days = 30            # also keep anything this new
repack_dead_ratio = 0.30  # rewrite a pack only when more than 30% dead
```

`unwreck gc --keep N` keeps only the newest N snapshots for that run.
