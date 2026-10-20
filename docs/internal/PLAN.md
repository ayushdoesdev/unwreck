# unwreck — build plan

> Undo for AI coding agents. Snapshot the working tree at every agent turn (including
> files changed by shell commands and files git doesn't track), and restore any turn
> with one command, without trampling edits you made afterwards.

Status: core loop works in `code/` (packs, stat cache, no-op reuse, parallel scan).
All facts below were checked on **2026-10-18**; anything not verified is marked **UNVERIFIED**.

---

## 1. Positioning

**One-liner:** `unwreck undo`: Ctrl+Z for Claude Code, Codex, Gemini CLI, aider, or any agent.

**The gap, from primary sources:**

| Fact | Source |
|---|---|
| Codex users asking for `/undo` back: open, 537 reactions, 93 comments | https://github.com/openai/codex/issues/9203 |
| Codex `/rewind` that reverts chat + code: open, 229 reactions, 48 comments | https://github.com/openai/codex/issues/11626 |
| Claude Code checkpointing does **not** track Bash changes (`rm`, `mv`, `cp`), most subagent edits, external edits, or directory create/move/delete | https://code.claude.com/docs/en/checkpointing and https://code.claude.com/docs/en/agent-sdk/file-checkpointing |
| Gemini CLI checkpointing exists but is **off by default** (needs `general.checkpointing.enabled`), uses a shadow git repo in `~/.gemini/history/<project_hash>`, triggered when you approve file-modifying tools such as `write_file`/`replace` | https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/checkpointing.md |

**Wedge:** one tool, every agent, covering the cases the built-ins miss: shell-command
damage, untracked and git-ignored files such as `.env`, and edits you made after the agent's
turn (conflict-aware restore).

**Non-goals (v0):** restoring chat or conversation state (each agent owns that), remote/cloud
sessions, replacing git, editor plugins.

### Direct competitors (checked 2026-10-18)

| Project | Stars | Lang | Last push | Notes |
|---|---|---|---|---|
| peaktwilight/agent-undo (`au`) | 7 | Rust | 2026-06-05 | **Closest competitor.** Same core idea: BLAKE3 + zstd + SQLite timeline, background daemon, Claude Code hook install, `au oops`. README lists macOS/Linux release binaries. Crate `agent-undo` published 2026-04-08 (34 downloads). Not tested by me. |
| MFaizR77/turnback (npm `turnback`) | 1 | TypeScript | 2026-10-14 | **Same problem, direct competitor.** npm v0.9.1 (2026-10-14). Describes itself as "Undo a coding agent turn, including shell commands and untracked files, via hooks, a CLI, and an MCP server". Lists Claude Code, Codex, Cursor, Gemini CLI, OpenCode and Antigravity; commands `undo`/`redo`/`blame`/`recover`. Not tested by me. |
| extracurricular-ai/codex-rewind | 54 | Rust | 2026-09-27 | Unofficial **fork/distribution of Codex** adding `/rewind` and `/redo`; Codex-only. |
| yesonsys03-web/VibeLign | 26 | Python | 2026-06-27 | "Checkpoints, undo, anchors, MCP, secret protection". |
| lintsinghua/oops | 14 | Go | 2026-10-14 | Ctrl+Z for agents; created 2026-10-13. |
| nirbenda/revertly | 11 | Python | 2026-08-16 | Records every file change, reverts. |
| mohshomis/ckpt | 7 | TypeScript | 2026-04-03 | Checkpoints on top of git. |
| Archerkattri/stepback, hichipli/takeback, trouties/bashback | 1–2 | – | 2026 | Small. |
| Rollcage (rollcage.app) | ? | ? | ? | Has a Claude Code undo guide; product/licence **UNVERIFIED**. |
| CaydenChik/doover, stordai, opendot (PyPI), Backboard CLI checkpoints | ? | ? | ? | Surfaced in a web search; **not yet evaluated**. |

So the field isn't empty. It is crowded with small, recent projects, and nobody has more than
54 stars. The two closest are `agent-undo` (Rust) and `turnback` (TypeScript). `turnback` already
lists more agents than we plan for v0 (Cursor, OpenCode and Antigravity too) and ships an MCP
server, which we don't. Against it we offer a single static binary with no Node runtime,
conflict-aware restore (whether `turnback` has an equivalent is unchecked), Windows CI, and
hook-time latency targets (§9). How we differ, all testable claims:
1. **Turn-level, not write-level.** Snapshots line up with agent turns (hook payload ids) and tool calls, so `undo` means "the last thing the agent did", not "the last write".
2. **Conflict-aware restore.** We never silently overwrite a file you edited after the last snapshot (see §6).
3. **No daemon required.** Hooks plus `unwreck run` cover the main agents. `watch` is optional.
4. **Windows from day one** in CI.
5. **Captures small git-ignored secrets** (`.env*`) while skipping `node_modules`/`target`.

---

## 2. Name

**Chosen: `unwreck`** (binary `unwreck`, 7 characters). Reads as a verb: the agent wrecked
the repo, so you `unwreck` it.

Why not the earlier pick `turnback`: npm `turnback` is MFaizR77/turnback, a tool for exactly the
same problem (see the competitor table in §1).

Availability of `unwreck` (checked 2026-10-18):

| Check | Result | How |
|---|---|---|
| crates.io `unwreck` | **free** (HTTP 404) | `GET https://crates.io/api/v1/crates/unwreck` |
| Homebrew core formula / cask `unwreck` | **free** (404 / 404) | `GET https://formulae.brew.sh/api/formula/unwreck.json`, `.../api/cask/unwreck.json` |
| npm `unwreck` | **free** (404) | `GET https://registry.npmjs.org/unwreck` |
| PyPI `unwreck` | **free** (404) | `GET https://pypi.org/pypi/unwreck/json` |
| GitHub user/org `unwreck` | **free** (404), so `github.com/unwreck` can be registered as an org | `GET https://api.github.com/users/unwreck` |
| Distro packages | **none** (0 Repology projects) | `GET https://repology.org/api/v1/projects/?search=unwreck` |
| Existing Unix command | none (`command -v unwreck` empty on the build box) | local |
| GitHub repos named `unwreck` | 3: Wpgradford/Unwrecked-Ships (1★), ejdhindsa/unwreck (0★, a Vue portfolio hub), ejdhindsa/unwreck-core (0★, design tokens for that brand). No dev tool. | GitHub search `unwreck in:name` |
| Web | `unwreck.dev` is **taken** (a personal portfolio hub tied to ejdhindsa/unwreck). Other hits: a Minecraft "Unwrecked Ships" datapack. No developer tool or product named unwreck found. | web search |

**Domain: none chosen yet.** `unwreck.dev` is taken. Other domains were not checked.
Runner-up names that were also free on every check above: `unruin`, `unyeet`.
**Reserve the names early:** the `unwreck` crate (publish 0.0.1 from the skeleton), the GitHub org
`unwreck`, and optionally an npm placeholder. Trademark search: **not done**.

Earlier crates.io checks on other candidates: free = `agundo`, `snapback`, `reverto`, `ctrlz`,
`takeback`, `agent-rewind`, `agentundo`, `undoit`, `unagent`, `rollback-agent`; taken = `rewind`,
`undo`, `agent-undo` (the competitor above), `oopsie`, `backstep`, `retrace`.

---

## 3. Architecture

Three ways to capture state, ranked by precision. All of them end in the same call:
`snapshot(root, trigger)`.

```
            ┌──────────── agent hooks (precise) ───────────┐
Claude Code │ UserPromptSubmit → TurnStart                 │
Codex       │ PreToolUse(*) → PreTool                         │──► unwreck hook <agent>  (stdin JSON)
Gemini CLI  │ Stop / AfterAgent → TurnEnd                  │          │
            └──────────────────────────────────────────────┘          ▼
unwreck run -- aider ...  (spawn child, watch FS, snapshot on quiet) ──► snapshot() ──► store
unwreck watch             (IDE agents: Cursor, etc.)               ──►     ▲
unwreck snap -m "..."     (manual pin)                              ──►     │
                                                              restore / undo / redo / diff / log / tui
```

### 3.1 Hook support, verified 2026-10-18

**Claude Code** (https://code.claude.com/docs/en/hooks):
- Events include `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolBatch`, `Stop`, `SubagentStart/Stop`, `SessionEnd`, `FileChanged`, `CwdChanged`, and more.
- Command hooks get JSON on stdin with `session_id`, `prompt_id`, `cwd`, `transcript_path`, `hook_event_name`. Tool events add `tool_name`, `tool_input`, `tool_use_id`. Subagent tool calls fire the same hooks and carry `agent_id`/`agent_type`.
- Matchers: `"Bash"`, `"Edit|Write"`, regex. An `if` field narrows further, e.g. `"Bash(rm *)"`.
- Exit 0 with no output means "no decision". Exit 2 blocks. Other codes are non-blocking errors. **We always exit 0 and print nothing on stdout** so we can never block or alter the agent.
- Default command-hook timeout is 600 s, lowered to 30 s on `UserPromptSubmit`. Matching hooks run **in parallel**, so the store needs a lock.
- Config locations: `~/.claude/settings.json`, `.claude/settings.json`, `.claude/settings.local.json`, plugin `hooks/hooks.json`.
- Important: a **PreToolUse hook on Bash runs before the shell command**, which is exactly what Claude Code's own checkpointing misses.

**Codex** (https://developers.openai.com/codex/hooks):
- Events: `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, `Stop`, `Interrupt`, `SessionStart`, `SubagentStart`, `SessionEnd`.
- Hooks are **on by default** (`[features] hooks = false` turns them off). Config lives in `~/.codex/hooks.json`, `~/.codex/config.toml`, or `<repo>/.codex/…`. Project-local hooks load only in trusted projects. **Non-managed hooks must be reviewed and trusted before they run**, so the user has to approve once.
- Tool coverage: shell commands and unified exec (matched as `Bash`), `apply_patch` (matchable as `apply_patch`, `Edit`, or `Write`), MCP tools, and other local function tools. Hosted tools such as WebSearch are not covered. The docs say to treat hooks as "a useful guardrail, not a complete enforcement boundary". Turn-scoped payloads include `turn_id`. **Live Codex (2026-10-18):** `PreToolUse` fired for `apply_patch` but not for `rm -rf` with matcher `Bash|apply_patch`. We install matcher `"*"` (documented catch-all: `"*"`, `""`, or omit).
- Multiple matching hooks launch concurrently.

**Gemini CLI** (https://geminicli.com/docs/hooks/reference/ and the configuration reference):
- `settings.json` (`~/.gemini/` or `.gemini/`) has `hooks.BeforeTool`, `AfterTool`, `BeforeAgent`, `AfterAgent`, `Notification`, `SessionStart`, `SessionEnd`, `PreCompress`.
- Only `type: "command"` is supported. Timeout is in **milliseconds** (default 60000). Tool matchers are regexes (tool names such as `run_shell_command`, `write_file`, `replace`).
- `hooksConfig.enabled` defaults to true.
- I did not read every payload field. **UNVERIFIED:** whether `BeforeAgent` carries the prompt text and a session id. The skeleton tolerates missing fields.

**aider, Cursor, others:** no hook research done (**UNVERIFIED**). They use `unwreck run` or `unwreck watch`.

### 3.2 Why not only a filesystem watcher?

- Watchers (inotify / FSEvents / ReadDirectoryChangesW through `notify`) report *after* the write, so the pre-change content is already gone unless we hold a previous snapshot. They also drop events under load: an inotify queue overflow becomes a rescan.
- So the design is **snapshot-diffing, not event-logging**. Every trigger runs an incremental full-tree scan, and the watcher only decides *when* to scan. Watcher bugs cost precision, never correctness.
- Hooks give exact pre-tool timing. `PreToolUse` on `Bash` captures state *before* `rm -rf`.

### 3.3 Process wrapper (`unwreck run -- <cmd>`)

- Spawns the child with inherited stdio (no PTY in v0, so the agent's TUI works unchanged). It snapshots at start, after every quiet period (default 1500 ms after activity), and at exit.
- Limitation: it can't see keystrokes, so turn boundaries are approximate.
- PTY interposition (`portable-pty` 0.9.0) to detect Enter is a v1 option, not MVP.

---

## 4. Snapshot store

Stored **outside the worktree**: `$XDG_DATA_HOME/unwreck/projects/<blake3(canonical root)[..16]>/`
(via `directories` 6.0.0; on macOS `~/Library/Application Support/dev.unwreck.unwreck/`;
on Windows `%APPDATA%\unwreck\unwreck\data\`; override with `UNWRECK_DIR`).
Reason: agents run `git clean -fdx` and `rm -rf .`, and an in-repo `.unwreck/` would die
with the code it protects. It also keeps snapshots out of the agent's context and out of git.

```
packs/pack-<n>.pack + .idx       small blobs (< 64 KiB compressed), one pack per snapshot batch (done)
objects/ab/<blake3-hex>.zst      loose blobs: ≥ 64 KiB compressed, and anything older builds wrote
manifests/<id:016>.json.zst      one per snapshot, sorted entries + empty dirs (format v2)
statcache.bin                    path → (size, mtime, ctime, inode, hash), checksummed (done)
LOCK                             std::fs::File::lock (stable since Rust 1.89)
tmp/                             write-then-rename staging
```

- **Content addressing:** BLAKE3 of raw bytes (`blake3` 1.8.7) is the object name. Identical content across files and snapshots is stored once (dedup is automatic). Every `get` re-hashes the blob to verify it.
- **Compression:** `zstd` 0.14.3, level 3. (M2) Store raw when compression saves < 5% (png/zip/jpg).
- **Manifest per snapshot:** `{version, id, created_unix_ms, trigger, entries:[{path, kind: file{hash,size,executable} | symlink{target} | oversize{size}}], empty_dirs:[path]}`. Sorted by path, so lookups are binary search and diffs are a linear merge. The manifest is written **last** (after its blobs are durable), so a crash leaves at most orphan data, which gc removes.
  - **Versioning:** `version` 2 adds `empty_dirs`. Manifests from the first skeleton have no `version` (read as 0) and still load. Restoring to one of them never prunes directories, because we can't tell which empty dirs it had.
- **Packs (done):** during a snapshot, every new blob under 64 KiB compressed is appended to one new pack file; bigger blobs go to temp files.
  - At commit: fsync the pack once, then write its `.idx` atomically (temp + fsync + rename). The index format is `magic | count | pack_len | (blake3, offset, len)* | blake3 checksum`.
  - Then rename the loose blobs into place, then write the manifest. That is one fsync for the pack, one for the index, one per large blob, one for the manifest, plus directory fsyncs. The old code did one fsync per blob.
  - **Crash and truncation rules:**
    - A pack without an `.idx` is an orphan: it's ignored and gc deletes it.
    - An index with a bad checksum is ignored, so its blobs read as missing rather than wrong.
    - Index entries that extend past the pack's actual length are dropped, so a truncated pack loses only the cut blobs, and the next snapshot re-stores them.
    - Trailing junk after the indexed region is ignored.
  - Every `get` still verifies the BLAKE3 hash.
  - All of these cases have unit tests in `unwreck-store`.
- **Stat cache (done):** `statcache.bin` maps path → (size, mtime s+ns, ctime s+ns, inode) → hash. A file whose stat matches, and whose blob still exists, is not read or rehashed.
  - **Racy-write rule:** an entry is only cached if the file's mtime is ≥ 2 s older than the scan start. A write in the same timestamp tick as a scan therefore can't be missed. The cost: files written in the last 2 s are always rehashed.
  - The cache is checksummed. A missing or damaged cache just means a full rehash; it is written atomically but not fsynced.
- **Measured on this box (8 vCPU, release build), 10,000 files / 40 MB (`code/scripts/bench.sh`):**

| | old skeleton (loose objects, no cache) | packs + stat cache | + no-op reuse + rayon (`BENCH_AGE=1`, 20 hook runs) |
|---|---|---|---|
| First snapshot | 2,013–2,336 ms | 223–251 ms | 200.6 ms |
| Unchanged re-snapshot, aged tree | 121–136 ms | 52–59 ms | p50 **27.6 ms**, p95 **28.4 ms** (`unwreck hook claude`) |
| Store files after first snap | thousands of loose objects | 7 files | 6 files (one manifest; no-ops add none) |

  Unchanged hook snapshots reuse the previous id via `tip.json` (tree fingerprint). Manual `snap` still writes. Next latency work: delta manifests (M3).
- **Dedup across snapshots** is free (same hash). Manifests for big repos will dominate. (M3) Delta manifests: store only changed entries plus a parent id, with a full checkpoint every 50.
- **GC / retention** (`unwreck gc`, plus opportunistically on `SessionEnd`): keep everything from the last 24 h, the turn-boundary snapshots from the last 14 days, and every manual pin. Cap the store at 2 GiB per project; past the cap, drop the oldest non-pinned snapshots first. Mark-and-sweep: collect hashes referenced by kept manifests, delete unreferenced loose objects, then **repack**. Repacking copies every live packed blob (raw compressed bytes) into one new pack, commits it, and only then deletes the old indexes and packs; orphan packs and stale temp files go too. A crash mid-gc leaves duplicates, never a missing live blob. Implemented in `unwreck-store::gc` (simple form: it rewrites all packs every time it runs).
- **Size caps:** a file over 16 MiB is recorded as `oversize{size}` (metadata only). Restore reports it as `MISSING … (was over the size cap)` instead of pretending. Configurable.
- **What gets captured:**
  - `.git/` is never captured (we never rewrite git internals; for refs, point users to `git reflog`).
  - `.gitignore`, `.ignore`, and the global excludes are respected (`ignore` 0.4.33 walker, hidden files *included*).
  - **But** small ignored files matching `always_include` (default `.env`, `.env.*`) are captured anyway, because an ignored `.env` is irreplaceable. When a `.gitignore` or `.ignore` exists, a second whitelist-only walk picks them up without re-hashing the rest of the tree.
  - Heavy directories (`node_modules`, `target`, `.venv`, `venv`, `__pycache__`, `dist`, `build`, `.next`, `.turbo`, `.gradle`) are never descended. They are rebuildable and would blow the latency budget.
  - (M2) `.unwreckignore` for overrides.
- **Symlinks:** never followed. The link target string is stored and restored as a link. On Windows, creating symlinks needs Developer Mode or the privilege; on failure, report it per file and continue.
- **Permissions:** only the executable bit (Unix). Owner, group, ACLs, and xattrs are not captured in v0. On restore, mtime is set to *now*, not the old mtime, so make/cargo/vite notice the change.
- **Binary files:** treated like any other blob. `diff` shows "binary, N bytes" (M2, using `similar` 3.2.0 for text).
- **Paths:** stored as repo-relative, `/`-separated UTF-8. Non-UTF-8 Unix paths are skipped with a warning in v0 (M3: store raw bytes).
- **Windows path concerns:**
  - Case-insensitive filesystems (and macOS APFS default): diff with case-folded keys, warn on case-only renames.
  - `MAX_PATH`: use `\\?\` verbatim paths for I/O and `dunce` 1.0.5 for display.
  - Reserved names (`CON`, `NUL`, `AUX`…) and trailing dots/spaces: refuse to restore with a clear error.
  - Files held open by editors or AV: sharing violations, so retry with backoff (3× / 50–200 ms) and then report.
  - `\r\n`: never normalized; bytes are bytes.
- **Atomicity of restore (done):** write to a sibling `.{name}.unwreck.tmp`, fsync, set mode (copy previous Unix bits, then apply the snapshot executable bit), rename over the target. Leftover temps are deleted on failure (Drop) and at the start of the next apply. Windows: clear read-only, retry remove+rename on sharing-violation / access-denied (3×, 50–200 ms). `.git` is still never touched.

---

## 5. Turn boundaries

| Source | Snapshot trigger | Precision |
|---|---|---|
| Claude Code `UserPromptSubmit` | `TurnStart{session_id, prompt_id, prompt}` | exact |
| Claude Code `PreToolUse` matcher `Bash\|Write\|Edit\|MultiEdit\|NotebookEdit` | `PreTool{tool, command/file_path}` | exact, before the change |
| Claude Code `Stop` | `TurnEnd` | exact |
| Codex `UserPromptSubmit` / `PreToolUse` (`*` matcher) / `Stop` | same, keyed by `turn_id` | exact (within Codex's documented tool coverage) |
| Gemini `BeforeAgent` / `BeforeTool` (`run_shell_command\|write_file\|replace`) / `AfterAgent` | same | exact (payload fields partly **UNVERIFIED**) |
| `unwreck run` / `watch` | `Quiescent` after N ms of no FS events following activity | approximate |
| `unwreck snap -m` | `Manual` (pinned in gc) | exact |

- `undo` targets the newest **turn-boundary** snapshot (`TurnStart`, `Quiescent`, or `Manual`) whose contents differ from the latest state. `PreTool` snapshots are fine-grained points you can restore explicitly (`unwreck restore <id>`). This works in the skeleton: in the smoke test, `undo` jumped over the `before Bash rm -rf` snapshot to the turn start.
- **Cheap no-op snapshots (done):** if the tree fingerprint matches `tip.json`, hook/watcher triggers reuse the previous snapshot id and write no manifest. Manual pins and restore bookkeeping always write. Unchanged 10k-file hook snapshot: p50 27.6 ms, p95 28.4 ms (20 runs, `BENCH_AGE=1`).
- **Latency budget for synchronous hooks:** p50 < 50 ms, p95 < 150 ms on a 10k-file repo (§9), because PreToolUse runs before every tool call.

---

## 6. Restore, redo, conflicts

Inputs: **T** = target snapshot, **L** = latest snapshot unwreck recorded, **C** = a fresh
`PreRestore` snapshot of the disk taken now (it doubles as the redo point).

For each file path in T ∪ C:

| Condition | Action |
|---|---|
| T == C | nothing |
| T is `oversize` | `MISSING` (can't restore contents) |
| C == L (untouched since unwreck last looked) | restore T, or delete the file if T doesn't have it |
| C != L (someone, usually you, edited it after the last snapshot) | **SKIP as conflict** unless `--force` |

**Directories (done, manifest v2):**
- Empty dirs recorded in T but missing now are recreated (`mkdir`).
- An empty dir in C that isn't in T is removed only if L also recorded it as empty. That's the same C == L rule as for files, so an empty dir you made after the last snapshot stays.
- After applying, ancestors of deleted paths that are now empty are removed (`rmdir`, using `remove_dir`, which refuses non-empty dirs), unless the dir exists in T.
- `.git` is never touched.
- A dir that holds a file you added later is never empty, so it's never removed.
- If T is a pre-v2 manifest, nothing is pruned.

After applying, a `PostRestore` snapshot records the new state. Without it, the next restore would
see every restored file as "changed since last" and call it a conflict. `redo` is just "restore
the newest `PreRestore` snapshot", so it is symmetric and reversible, and `--force` is always
recoverable. All of this is implemented and smoke-tested in the skeleton:

```
$ unwreck undo
  restore .env
  restore src/gen/x.rs
  restore src/lib.rs
restored 3 paths to #1; your previous state is #8 (`unwreck redo`)
$ echo "// my manual fix" >> src/lib.rs
$ unwreck redo
  delete  .env
  delete  src/gen/x.rs
  SKIP    src/lib.rs  (changed after last snapshot; --force to overwrite)
  rmdir   src/gen/  (emptied by this restore)
restored 2 paths to #8; your previous state is #10 (`unwreck redo`)
```

Planned extensions:
- `--merge` (M3): three-way text merge of the conflicting file using `diffy` 0.5.2 (base L, ours C, theirs T), writing conflict markers.
- `--only <path>…` (implemented): restore selected files.
- `unwreck revert <id>` (M3): undo just one turn's delta on top of later turns, i.e. a three-way merge per file, like `git revert`.
- `.git` is never touched. If the agent ran `git reset --hard`, unwreck restores the files and the user fixes refs with `git reflog`.

---

## 7. Workspace layout (versions = max stable on crates.io, 2026-10-18)

MSRV **1.90**. Needed because `redb` 4.4.0 needs 1.90, and `ignore` 0.4.33 and `ratatui` 0.30.2
need 1.88. The box's distro Rust was 1.85.1, so the skeleton was checked with rustup stable **1.99.1**.

| Crate | Role | Key deps (version) |
|---|---|---|
| `unwreck-core` | types: `ContentHash`, `Entry`, `Manifest`, `Trigger`, path normalization | `serde` 1.0.229, `thiserror` 2.0.21 |
| `unwreck-store` | packs + index, loose objects, manifests, lock, gc with repack | `blake3` 1.8.7, `zstd` 0.14.3, `serde_json` 1.0.154 |
| `unwreck-scan` | walk → manifest (files + empty dirs); ignore policy; stat cache; parallel walk/hash; no-op reuse | `ignore` 0.4.33, `rayon` 1.12.0 |
| `unwreck-watch` | quiescence detection | `notify` 8.2.0, `notify-debouncer-full` 0.7.0 |
| `unwreck-hooks` | parse Claude/Codex/Gemini payloads; config snippets; merge/remove `hooks --write`/`--remove` | `serde_json` |
| `unwreck-restore` | plan (T/L/C), apply, directory mkdir/prune, (M3) merge | `diffy` 0.5.2 (M3) |
| `unwreck-tui` | timeline picker, diff pane (M3) | `ratatui` 0.30.2 (re-exports crossterm 0.29.0) |
| `unwreck` (bin) | CLI | `clap` 4.6.7 (derive), `anyhow` 1.0.104, `directories` 6.0.0 |
| dev | tests | `tempfile` 3.27.0, `assert_cmd` 2.2.2, `insta` 1.49.0, `proptest` 1.12.0, `criterion` 0.8.2 |
| optional | index DB if JSON manifests get slow | `redb` 4.4.0 (pure Rust) or `rusqlite` 0.40.2 |
| release | installers, brew tap, curl script | `cargo-dist` 0.32.0 (axodotdev/cargo-dist, 2,135★, pushed 2026-10-14) |

---

## 8. CLI spec

```
unwreck hooks [<claude|codex|gemini>] [--write|--remove] [--agent …] [--scope project|user] [--dry-run]
unwreck hook  <claude|codex|gemini>        hook entry point; reads stdin JSON; ALWAYS exit 0, silent stdout
unwreck run [--quiet-ms 1500] -- <cmd…>    wrap any agent
unwreck watch [--quiet-ms 1500]            snapshot on quiescence
unwreck snap [-m <msg>]                    manual pin
unwreck log [-n 20]                        timeline
unwreck diff [<from> [<to>]] [--patch]     no args = last turn-start vs disk; one id vs disk; two ids
unwreck restore <id> [--only P…] [--force] [--dry-run]
unwreck undo [--force] [--dry-run]         back to the previous turn boundary
unwreck redo [--force]                     undo the last restore
unwreck tui                                timeline picker (enter = restore)
unwreck gc [--keep N]                      retention (M2: --max-size, time policy)
unwreck doctor [--json]                    store, hooks, agent CLIs, git; exit 1 only on ✗
```

Real output from the skeleton's smoke test (Claude Code payloads piped by hand):

```
$ unwreck log
#1       0s ago     4 changed  claude turn start: "clean up generated code"
#2       0s ago     0 changed  claude before Bash `rm -rf src/gen .env`
#3       0s ago     3 changed  claude turn end
$ unwreck diff
D .env
D src/gen/x.rs
M src/lib.rs
$ unwreck undo --dry-run
  restore .env
  restore src/gen/x.rs
  restore src/lib.rs
dry run: nothing changed
```

---

## 9. Performance targets (to be measured with `criterion` + hyperfine in CI)

| Scenario | Target |
|---|---|
| Hook snapshot, 10k-file repo, nothing changed | p50 ≤ 30 ms, p95 ≤ 80 ms (**met** on this box: p50 27.0 ms, p95 28.1 ms over 20 `hook claude` runs, aged 10k-file tree, release) |
| Hook snapshot, 10k files, 20 changed | p50 ≤ 50 ms, p95 ≤ 150 ms |
| First snapshot, 10k files / 40 MB | ≤ 600 ms (now 200.6 ms on the same bench; was 223–251 ms before no-op/rayon, 2,013–2,336 ms on the old skeleton) |
| Linux kernel–sized tree (~90k files) unchanged | ≤ 400 ms (**UNVERIFIED** feasibility; measure) |
| Restore 100 files | ≤ 100 ms |
| Store overhead for 1 day of heavy use | ≤ 2× repo source size |
| Binary size (release, stripped) | ≤ 8 MB |
| Idle `watch` CPU | ≈ 0% |

---

## 10. Testing strategy

- **Tests (50 passing, including 1 doctest):** hash serde + tree fingerprint, store put/get/dedup/gc/packs (30% dead repack), scan policy, restore plan/apply, hook parsing + docs fixtures + CLI exit 0, hook config merge/remove, doctor, proptest restore round-trip, abort-during-snap, `unwreck run` wrap.
- **Property tests (`proptest`):** random trees and random edit sequences. Invariants:
  - `restore(T)` with no interleaved user edits makes disk == T.
  - restore followed by redo gives back the pre-restore disk exactly.
  - no path changed after L is ever overwritten without `--force`.
- **CLI golden tests (`assert_cmd` + `insta`):** `log`, `diff`, `restore --dry-run` output.
- **Hook contract tests:** recorded real payloads from each agent version (capture fixtures with a logging hook). Assert exit 0, empty stdout, and < 150 ms, even with bad JSON or a locked store (the skeleton already exits 0 on bad JSON).
- **Agent end-to-end (nightly, manual at first):** scripted Claude Code (`claude -p`), Codex exec mode (**UNVERIFIED** flags), and Gemini on a fixture repo, each doing `rm -rf` plus edits, then `unwreck undo`.
- **Crash safety:** kill -9 during snapshot and during restore at random points (fault-injection feature flag), then `unwreck fsck` (M3) finds only orphan objects.
- **CI matrix:** ubuntu, macos, windows; ext4, APFS (case-insensitive), NTFS. Windows-specific tests: long paths, reserved names, a locked file during restore, symlink without privilege.
- **Benchmarks:** criterion on synthetic 10k- and 90k-file trees; fail CI on > 20% regression.

---

## 11. Risks

| Risk | Mitigation |
|---|---|
| Codex or Claude Code ship a full undo (bash-aware). Codex #9203 is open, so a native return of `/undo` is plausible. | Stay agent-agnostic, cross-tool, conflict-aware; the value survives any single vendor fixing it. |
| Competitors in the same space (`agent-undo`/`au` in Rust; `turnback` on npm, which lists more agents) | Ship faster, Windows, conflict safety, better demo; keep claims honest. |
| Hook latency annoys users | Stat cache, no-op detection, background mode (Claude `async: true` exists, but async snapshots lose "before" semantics; offer it only for `Stop`). |
| Disk bloat | Packs, size caps, heavy-dir skips, gc defaults, `unwreck du` (M3). |
| Capturing secrets (`.env`) into another location | Store is in the user's data dir with 0700/0600 perms (`Store::permission_warnings` for doctor); document it; `always_include` is opt-out. Never sync or upload anything. |
| Restore destroys work | Conflict rule plus mandatory PreRestore snapshot; `--force` stays recoverable; `--dry-run` everywhere. |
| Codex hooks require a trust review; Gemini/Claude config formats change | `unwreck doctor` (M3) checks that hooks are installed and firing; contract fixtures per agent version. |
| Windows edge cases | CI from week 1; clear per-file errors rather than aborting. |
| Name squatting or a clash (`unwreck.dev` is already taken by a portfolio) | Reserve the crate, the GitHub org `unwreck` and npm now; pick a different domain. |

---

## 12. Four-week plan

**Week 1: core works end to end (skeleton already covers most of it)**
- [x] Workspace, store (objects, manifests, lock, gc), scan with ignore policy, restore planner with conflicts, hook parsing, CLI (`snap/log/diff/restore/undo/redo/hook/hooks/run/watch/tui/gc`).
- [ ] Publish `unwreck` 0.1.0 to crates.io (needs credentials); GitHub repo + CI (3 OS) are in-tree, not pushed.
- [x] Versioned hook fixtures from vendor docs (labeled “from docs, not captured”); `scripts/capture-hooks.sh` for live payloads.

**Week 2: fast and safe**
- [x] Stat cache; packs for small objects with checksummed index; one fsync per snapshot batch; gc repack.
- [x] Empty-dir pruning and empty-dir tracking (manifest v2).
- [x] No-op snapshot skipping; rayon parallel walk/hashing. Unchanged 10k-file hook snapshot p50 27.3 ms / p95 27.7 ms (20 runs, `BENCH_AGE=1`, release).
- [x] Atomic restore (temp + rename), Windows retries, 0700 store perms (`Store::permission_warnings` for `doctor`).
- [x] `unwreck hooks --write`/`--remove` for Claude, Codex, and Gemini (backup `.unwreck.bak`, merge, idempotent, `--dry-run` diff); `unwreck doctor` (`--json`, exit 1 only on ✗).
- [x] proptest snapshot→edit→restore round-trip; abort (`UNWRECK_INJECT_CRASH=after_blobs`) during snapshot write.

**Week 3: UX that demos well**
- [x] `diff --patch` (`similar`), `NO_COLOR`-aware color, `unwreck tui` with file list + unified diff (enter/u/r/q).
- [ ] `restore --merge` (diffy); `revert <id>`.
- [x] gc retention (`keep_turns` / `keep_days` in config); packs rewritten only when >30% dead. `unwreck du` still M3 (doctor prints disk usage).
- [x] cargo-dist config + GitHub Release workflow (musl/mac/Windows) and shell/PowerShell installers. Homebrew tap still optional.

**Week 4: launch**
- [ ] Record the demo GIF (LAUNCH.md script), finalize README, docs site (optional).
- [ ] Dogfood for 3 days on real Claude Code and Codex sessions; fix papercuts.
- [ ] Launch per LAUNCH.md; triage issues daily.

---

## 13. Not verified / open questions

- Gemini CLI payload fields for `BeforeAgent`/`AfterAgent` (prompt, session id).
- Whether Gemini's own checkpoint covers `run_shell_command` changes (the docs mention `write_file`/`replace`).
- Codex non-interactive flags for end-to-end tests.
- What Codex's original `/undo` did and why it was removed (only the issue title and body were read).
- aider/Cursor hook mechanisms.
- How peaktwilight/agent-undo behaves (README read, not run); Rollcage's licence and pricing.
- Trademark availability of "unwreck"; which domain to use (`unwreck.dev` is taken, nothing else checked).
- Competitors not yet evaluated: CaydenChik/doover, stordai, opendot, Backboard CLI checkpoints.
- Performance targets beyond the single 10k-file measurement are goals, not measurements.
