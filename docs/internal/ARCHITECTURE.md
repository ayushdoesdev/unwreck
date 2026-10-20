# unwreck architecture

This is the implementation map for the crate layout in `code/`. Product intent and
milestones live in [PLAN.md](PLAN.md).

## Data flow

Every capture path ends in `unwreck_scan::snapshot(root, store, policy, trigger)`:

```
Claude / Codex / Gemini
  stdin JSON  →  unwreck-hooks::parse  →  Trigger
unwreck run / watch
  FS quiet    →  Trigger::Quiescent
unwreck snap
  CLI         →  Trigger::Manual

                    │
                    ▼
        Store::lock()  (exclusive LOCK file)
                    │
                    ▼
     walk tree (ignore + parallel) ──► stat cache hit? ─► reuse hash
                    │                         no
                    ▼                         ▼
            empty dirs + entries      rayon: read + BLAKE3
                    │                         │
                    ▼                         ▼
         tree fingerprint             Store::put_hashed (pack / loose)
                    │
        fingerprint == tip.json and trigger.allows_noop_reuse()?
           yes: reuse previous snapshot id (no new manifest)
            no: commit blobs, write manifests/<id>.json.zst, update tip.json
```

Restore:

```
unwreck restore / undo / redo
  T = target manifest
  L = latest recorded snapshot
  C = fresh PreRestore snapshot of disk (also the redo point)
       │
       ▼
  unwreck-restore::plan(T, L, C)   →  Write / Delete / Conflict / MkDir / RmDir
       │
       ▼
  apply: sibling temp → fsync → rename (never `.git`; leftovers swept next run)
       │
       ▼
  PostRestore snapshot of the new disk
```

`undo` picks the newest **turn-boundary** snapshot (`TurnStart`, `Quiescent`, `Manual`)
whose tree differs from the latest. `redo` restores the newest `PreRestore`.

## Crate boundaries

| Crate | Owns |
|---|---|
| `unwreck-core` | `ContentHash`, `Entry`, `Manifest`, `Trigger`, path + tree fingerprint |
| `unwreck-store` | Packs/index, loose objects, manifests, `LOCK`, gc/repack, `tip.json` |
| `unwreck-scan` | Ignore policy, stat cache, parallel walk, hashing, no-op reuse |
| `unwreck-restore` | T/L/C plan and apply |
| `unwreck-hooks` | Vendor JSON → `HookEvent`; snippet print; merge/remove of agent config files |
| `unwreck-core` config | `~/.config/unwreck/config.toml` overlay `.unwreck.toml` (`gc.keep_turns` 500, `keep_days` 30) |
| `unwreck-watch` | Quiescence detection (`notify`) |
| `unwreck-tui` | Timeline picker |
| `unwreck` | CLI (`anyhow`). Library crates use `thiserror`. |

The snapshot store is **outside** the worktree (`UNWRECK_DIR` or the platform data dir),
keyed by a BLAKE3 of the canonical project root. Agents that `rm -rf` the repo cannot
delete it. Existing manifests (format v0 without `version`, and v2 with `empty_dirs`)
still load. Extra files (`tip.json`, `statcache.bin`, packs) are ignored by old readers.

## Store format

```
<store>/
  LOCK
  tip.json                 {id, created_unix_ms, trigger, tree}   # rebuildable
  statcache.bin            path → stamp → hash, checksummed       # rebuildable
  manifests/<id:016>.json.zst
  packs/pack-<n>.pack      concatenated zstd blobs (< 64 KiB compressed)
  packs/pack-<n>.idx       magic UWIDX v1 | count | pack_len | (hash, off, len)* | BLAKE3
  objects/ab/<hash>.zst    loose blobs (≥ 64 KiB compressed, or pre-pack stores)
  tmp/                     write-then-rename
```

- Object name = BLAKE3 of **raw** bytes. `get` decompresses and re-hashes.
- A pack without a valid `.idx` is an orphan (ignored; `gc` deletes it).
- Index checksum mismatch → pack ignored (blobs look missing, never wrong).
- Index rows past the pack’s actual length are dropped (truncation).
- Manifest is written **last**, after blobs are durable.
- `tip.json` is not part of the snapshot identity. A missing tip falls back to
  decompressing the latest manifest.

No-op reuse (hook/watcher triggers only): if the scanned tree’s fingerprint matches
`tip.tree`, no new manifest is written. Manual pins and `PreRestore`/`PostRestore`
always write, so gc pins and redo stay correct.

## Restore algorithm

For each path in `T ∪ C` (optionally filtered by `--only`):

| Condition | Action |
|---|---|
| `T == C` | skip |
| `T` is `oversize` | `Unrecoverable` |
| `C == L` (untouched since last snapshot) | restore `T`, or delete if `T` has no path |
| `C != L` | `Conflict` unless `--force` |

Directories (manifest v2+): recreate empty dirs from `T`; remove empty dirs from `C`
only if `L` also recorded them empty; then `rmdir` ancestors emptied by this restore,
never `.git`, never a dir that still has files (including files the user added later).
Pre-v2 targets do not prune directories.

`--force` can still be undone: the mandatory `PreRestore` snapshot is the redo point.

Each file write is a sibling `.{name}.unwreck.tmp`, fsynced, then renamed into place. A crash
leaves the destination as either the old file or the new file, never a truncated mix. The next
`apply` deletes leftover temps.

The store directory is created `0700` and its files `0600` on Unix (`Store::permission_warnings`
is what `unwreck doctor` prints). Secrets such as `.env` live there.

`gc` deletes manifests outside the keep set (newest `keep_turns` snapshots, plus anything
newer than `keep_days`, unless `--keep N` caps to N). Pack files are rewritten only when
more than 30% of their compressed bytes are unreferenced.

## Hook install

`unwreck hooks --write` merges `unwreck hook <agent>` into the vendor JSON without dropping
the user's other hooks, writes a sibling `*.unwreck.bak` first, and is a no-op the second
time. `--remove` strips our commands and restores the backup bytes when the parsed backup
still matches. `--dry-run` prints a unified diff. Doc URLs (and anything unverified) are
in `unwreck-hooks/src/install.rs`. Codex `PreToolUse` is installed with matcher `"*"` (docs
allow `"*"`, `""`, or omit; a live session did not fire for `rm -rf` with `Bash|apply_patch`).
Re-running `--write` upgrades an older matcher. Codex still requires a one-time trust review of a new hook.
