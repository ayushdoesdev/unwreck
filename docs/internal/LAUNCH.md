# unwreck launch plan

Goal: the first 1,000 GitHub stars come from people who have actually lost files to an agent.
Every claim in launch copy has to match README.md. No "first" or "only" claims: there are
small competitors (see PLAN.md §1).

## T-7 days: readiness checklist

- [ ] `cargo install unwreck`, the brew tap, and the curl/PowerShell installers work on clean macOS (arm64), Ubuntu, and Windows VMs.
- [ ] `unwreck hooks claude|codex|gemini --write` tested against current releases of each agent. Record the versions in the README.
- [ ] Hook latency measured and published in the README (p50/p95 on a 10k-file repo). Fix anything over the PLAN.md §9 targets first.
- [ ] Demo GIF (below) ≤ 3 MB, plus an MP4 for X. Uses asciinema + agg, or VHS (charmbracelet/vhs) for a scripted, reproducible recording. Check that these tools are current before relying on them.
- [ ] README comparison table re-checked against each vendor's docs **on launch day** (Codex may re-add `/undo`).
- [ ] Issue templates: "lost files / wrong restore" (top priority), agent hook breakage, Windows.
- [ ] `CONTRIBUTING.md`, `good first issue` labels (e.g. aider integration, `.unwreckignore`, `du`).
- [ ] Crate name reserved, repo public, social preview image set.
- [ ] Dogfood: 3 days of real Claude Code and Codex use with the hooks on.

## Demo GIF script (≈25 s, 100×28 terminal, large font)

1. `cat .env` → `OPENAI_API_KEY=sk-demo…` (fake). `ls src/generated` shows 3 files. (3 s)
2. Start the agent with hooks installed and prompt: *"clean up the generated code and reset env config"*. (4 s)
3. The agent runs `rm -rf src/generated .env` and rewrites `src/app.ts`. Show the tool call line. (5 s)
4. `ls src/generated` → no such file; `cat .env` → No such file. A pause for dread. (3 s)
5. `unwreck log`: three lines, "turn start", "before Bash `rm -rf src/generated .env`", "turn end". (3 s)
6. `unwreck undo` → restore lines → `cat .env` is back. (4 s)
7. End card: `cargo install unwreck` · works with Claude Code, Codex, Gemini, any CLI. (3 s)

Variant GIF B (conflict safety, for the r/rust and HN crowd): edit a file by hand after
the agent's turn, run `unwreck redo`, and show the `SKIP … changed after last snapshot` line.

Use a fixture repo, never real keys. Script it with VHS so it can be re-recorded after UI changes.

## Where to post (in this order)

| # | Where | Angle | Notes |
|---|---|---|---|
| 1 | **Show HN** | engineering story: "snapshot-diff, not event log", why hooks run *before* Bash, conflict rule | Post Tue–Thu morning US Eastern (common practice, not a measured optimum). Reply to every comment for the first 3–4 h. |
| 2 | **openai/codex#9203** ("Please make /undo back", open, 537 reactions as of 2026-10-18) | one helpful comment: a workaround that works today with Codex hooks | Only post if it really works with current Codex. One comment, with disclosure; don't bump. |
| 3 | **openai/codex#11626** (`/rewind`, open, 229 reactions) | same; note it restores code, not chat | Same rules. |
| 4 | **r/ClaudeAI** | "Claude Code's /rewind doesn't cover Bash changes (per the docs). Here's a hook-based fix" | Link the docs page. GIF inline. Check the sub's self-promotion rules first (**not checked**). |
| 5 | **r/rust** | technical write-up: content-addressed store, `ignore` overrides for `.env`, std `File::lock`, Windows quirks | Rust community posts tend to do better as "what I learned building X". Check rules. |
| 6 | r/ChatGPTCoding, r/commandline, r/LocalLLaMA (aider users) | `unwreck run -- aider` | Space these over 1–2 weeks. |
| 7 | **X / Bluesky** | 20-second MP4 + one line + link; reply to viral "agent deleted my files" posts with the GIF, but only where it's relevant | Tag nobody unprompted. |
| 8 | This Week in Rust (submit to the "Project/Tooling Updates" section), Rust users forum | blog post | |
| 9 | Awesome lists: awesome-claude-code style lists, awesome-rust (CLI section), awesome-cli-apps | PR after 1–2 weeks of stability | Check each list's inclusion criteria. |

Never post in many places on the same hour with identical copy. Space them over 7–10 days so each
spike feeds the next (GitHub trending, then newsletters).

## Show HN title options (≤ 80 chars, no superlatives)

1. `Show HN: Unwreck – Ctrl+Z for AI coding agents, including what /rewind misses`
2. `Show HN: Unwreck – undo any Claude Code/Codex/Gemini turn, even rm -rf`
3. `Show HN: Unwreck – per-turn snapshots for coding agents (Rust, single binary)`
4. `Show HN: I kept losing untracked files to coding agents, so I built undo`

First comment (draft):
> Claude Code's checkpoints don't track changes made through Bash, according to its docs, and
> Codex users have been asking for `/undo` back (openai/codex#9203). unwreck hooks into each
> agent's PreToolUse/UserPromptSubmit/Stop events and snapshots the tree (BLAKE3 + zstd,
> stored outside the repo) before every shell command and at every turn. `unwreck undo` restores
> the last turn and skips any file you edited afterwards unless you pass `--force`. Every restore
> can be undone with `redo`. Known limits: no chat restore, files over 16 MiB are only recorded,
> and `.git` is never touched. Numbers: <p50/p95 hook latency on N-file repo>. Similar
> projects: agent-undo, codex-rewind, ckpt. Happy to compare.

## Draft GitHub issue comment (Codex #9203 / #11626)

> Until this lands natively, a workaround using Codex's hooks: `unwreck` snapshots the
> working tree on `UserPromptSubmit` and on every `PreToolUse` (matcher `*`, including shell), so it also
> covers files deleted by shell commands and untracked files. `unwreck undo` restores the last
> turn, and files you edited by hand afterwards are skipped. It restores code only, not chat.
> Disclosure: I wrote it. <repo link> (MIT/Apache)

(This is externally visible. Post it yourself, once per issue, only after testing with current Codex.)

## Metrics to track

**Daily for the first 2 weeks, then weekly:**
- GitHub: stars/day, unique visitors and clones (Insights → Traffic), referrers (HN vs Reddit vs X).
- Installs: crates.io downloads, GitHub release asset downloads per platform, brew tap installs (only if you add analytics; the binary has **no telemetry**, and keep it that way).
- Activation: issues mentioning a successful restore vs "restore did something wrong". The second kind is P0.
- Hook health: reports of slowdowns; median latency from users' `unwreck doctor` output (opt-in paste).
- Show HN rank/points and comment count; Reddit upvotes per post (upvote ratio).
- Retention signal: second-week issues and PRs from non-launch-day users; GitHub "used by"/dependents.

**Success bar:** ≥ 300 stars in week 1, ≥ 10 external issues, 0 data-loss bugs unresolved for more
than 24 h. These are targets, not predictions.
