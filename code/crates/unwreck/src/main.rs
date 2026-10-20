//! unwreck CLI. See PLAN.md "CLI spec".
mod doctor;
mod patch;
mod style;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unwreck_core::config::{self, Config};
use unwreck_core::{Manifest, Trigger};
use unwreck_hooks::install::{self, InstallPaths, Scope};
use unwreck_hooks::{Agent, HookEvent};
use unwreck_restore::Action;
use unwreck_scan::ScanPolicy;
use unwreck_store::Store;
use unwreck_tui::{DiffSource, Outcome};

#[derive(Parser)]
#[command(
    name = "unwreck",
    version,
    about = "Undo for AI coding agents: snapshot every turn, restore any of them.",
    after_help = "Examples:\n  unwreck hooks --write --agent claude --scope user\n  unwreck run -- aider\n  unwreck undo --dry-run\n  unwreck doctor\n\nColor is disabled when stdout is not a TTY or when NO_COLOR is set."
)]
struct Cli {
    /// Project root (defaults to the enclosing git worktree, else the current dir).
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum AgentArg {
    Claude,
    Codex,
    Gemini,
}
impl From<AgentArg> for Agent {
    fn from(a: AgentArg) -> Self {
        match a {
            AgentArg::Claude => Agent::Claude,
            AgentArg::Codex => Agent::Codex,
            AgentArg::Gemini => Agent::Gemini,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum AgentSel {
    Claude,
    Codex,
    Gemini,
    All,
}

#[derive(Clone, Copy, ValueEnum, Default)]
enum ScopeArg {
    #[default]
    Project,
    User,
}
impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Project => Scope::Project,
            ScopeArg::User => Scope::User,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Take a snapshot now (always written, even if the tree is unchanged).
    Snap {
        /// Stored on the snapshot as a log label.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// List snapshots, newest last.
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Diff snapshots. No ids: last turn-start vs disk. One id: that snapshot vs disk. Two ids: N vs M.
    #[command(
        after_help = "Examples:\n  unwreck diff\n  unwreck diff 12\n  unwreck diff 12 15 --patch"
    )]
    Diff {
        /// Snapshot id. Omit both ids to compare the last turn-start snapshot to disk.
        #[arg(value_name = "FROM")]
        from: Option<u64>,
        /// Later snapshot id. Omit to compare against the current working tree.
        #[arg(value_name = "TO", requires = "from")]
        to: Option<u64>,
        /// Unified diff of file contents (text files).
        #[arg(long)]
        patch: bool,
    },
    /// Make the working tree match snapshot ID. Never overwrites files you edited after the last snapshot unless `--force`.
    Restore {
        id: u64,
        /// Restore only these paths.
        #[arg(long = "only")]
        only: Vec<String>,
        /// Overwrite files that changed after unwreck last saw them (still recoverable via `redo`).
        #[arg(long)]
        force: bool,
        /// Print actions including mkdir/rmdir; do not write.
        #[arg(long)]
        dry_run: bool,
    },
    /// Roll back the most recent agent turn.
    Undo {
        #[arg(long)]
        force: bool,
        /// Print actions including mkdir/rmdir; do not write.
        #[arg(long)]
        dry_run: bool,
    },
    /// Undo the last restore/undo.
    Redo {
        #[arg(long)]
        force: bool,
    },
    /// Universal fallback: run any agent CLI (aider, Cursor CLI, …) and snapshot on quiet periods.
    #[command(
        after_help = "Example:\n  unwreck run --quiet-ms 1500 -- aider --yes\n  unwreck run -- sh -c 'echo demo'"
    )]
    Run {
        #[arg(long, default_value_t = 1500)]
        quiet_ms: u64,
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Watch the tree and snapshot on quiescence (for IDE agents without a CLI to wrap).
    Watch {
        #[arg(long, default_value_t = 1500)]
        quiet_ms: u64,
    },
    /// Entry point for agent hooks: reads the hook JSON on stdin. Always exits 0.
    Hook { agent: AgentArg },
    /// Print or install hook config for an agent.
    Hooks {
        /// With no `--write`/`--remove`, print a snippet for this agent.
        agent: Option<AgentArg>,
        /// Merge unwreck's hooks into the agent's config (idempotent).
        #[arg(long)]
        write: bool,
        /// Remove unwreck's hooks (restores `.unwreck.bak` bytes when it still matches).
        #[arg(long)]
        remove: bool,
        /// Which agent(s) to install/remove. Defaults to all when `--write`/`--remove`.
        #[arg(long = "agent", value_enum)]
        agent_flag: Option<AgentSel>,
        /// User-wide (`~/.claude/…`) or project-local (`.claude/…`). Default: project.
        #[arg(long, value_enum, default_value_t = ScopeArg::Project)]
        scope: ScopeArg,
        /// Print a unified diff; do not write.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check the store, hooks, agent CLIs, and git layout.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Interactive timeline with a file list and unified diff (enter restore, u undo, r redo).
    Tui,
    /// Delete old snapshots and unreferenced blobs. Packs are rewritten only if more than 30% dead.
    Gc {
        /// Keep the newest N snapshots (overrides config `gc.keep_turns`, default 500).
        #[arg(long)]
        keep: Option<usize>,
    },
}

fn main() {
    let cli = Cli::parse();
    // Hooks must never break the agent: swallow every error, exit 0, print nothing on stdout.
    if let Cmd::Hook { agent } = cli.cmd {
        if let Err(e) = hook(cli.root.as_deref(), agent.into()) {
            eprintln!("unwreck: hook skipped: {e:#}");
        }
        return;
    }
    if let Err(e) = real_main(cli) {
        eprintln!("unwreck: {e:#}");
        std::process::exit(1);
    }
}

fn real_main(cli: Cli) -> Result<()> {
    let root = project_root(cli.root.as_deref())?;
    match &cli.cmd {
        Cmd::Hooks {
            agent,
            write,
            remove,
            agent_flag,
            scope,
            dry_run,
        } => {
            return cmd_hooks(
                &root,
                *agent,
                *write,
                *remove,
                *agent_flag,
                (*scope).into(),
                *dry_run,
            );
        }
        Cmd::Doctor { json } => {
            let store = open_store(&root)?;
            let report = doctor::run(&root, &store, *json)?;
            if report.has_fail() {
                std::process::exit(1);
            }
            return Ok(());
        }
        _ => {}
    }
    let store = open_store(&root)?;
    match cli.cmd {
        Cmd::Snap { message } => {
            let m = snap(&root, &store, Trigger::Manual { label: message })?;
            println!("snapshot #{} ({} files)", m.id, m.entries.len());
        }
        Cmd::Log { limit } => log(&store, limit)?,
        Cmd::Diff { from, to, patch } => {
            let from_id = match from {
                Some(id) => id,
                None => last_turn_start(&store)?,
            };
            let a = store.read_manifest(from_id)?;
            let b = match to {
                Some(t) => store.read_manifest(t)?,
                None => scratch_scan(&root)?,
            };
            for (status, p) in diff(&a, &b) {
                println!("{} {p}", style::status(status));
                if patch && !p.ends_with("/ (empty dir)") {
                    print!("{}", patch::unified(&store, &a, &b, &p));
                }
            }
        }
        Cmd::Restore {
            id,
            only,
            force,
            dry_run,
        } => restore(&root, &store, id, only, force, dry_run)?,
        Cmd::Undo { force, dry_run } => {
            let target = undo_target(&store)?;
            restore(&root, &store, target, vec![], force, dry_run)?;
        }
        Cmd::Redo { force } => {
            let ids = store.list()?;
            let pre = ids
                .iter()
                .rev()
                .map(|&id| store.read_manifest(id))
                .find(|m| {
                    m.as_ref()
                        .is_ok_and(|m| matches!(m.trigger, Trigger::PreRestore { .. }))
                })
                .context("nothing to redo")??;
            restore(&root, &store, pre.id, vec![], force, false)?;
        }
        Cmd::Run { quiet_ms, command } => run(&root, &store, quiet_ms, &command)?,
        Cmd::Watch { quiet_ms } => {
            let (_tx, rx) = std::sync::mpsc::channel();
            watch_loop(&root, &store, quiet_ms, None, rx)?;
        }
        Cmd::Hooks { .. } | Cmd::Doctor { .. } => unreachable!(),
        Cmd::Tui => {
            let rows = timeline(&store, usize::MAX)?;
            let mut prev_map = std::collections::HashMap::new();
            let mut last = None;
            for r in &rows {
                if let Some(p) = last {
                    prev_map.insert(r.id, p);
                }
                last = Some(r.id);
            }
            let src = SnapDiffs {
                store: &store,
                prev: prev_map,
            };
            match unwreck_tui::run(&rows, &src)? {
                Outcome::Restore(id) => restore(&root, &store, id, vec![], false, false)?,
                Outcome::Undo => {
                    let target = undo_target(&store)?;
                    restore(&root, &store, target, vec![], false, false)?;
                }
                Outcome::Redo => {
                    let ids = store.list()?;
                    let pre = ids
                        .iter()
                        .rev()
                        .map(|&id| store.read_manifest(id))
                        .find(|m| {
                            m.as_ref()
                                .is_ok_and(|m| matches!(m.trigger, Trigger::PreRestore { .. }))
                        })
                        .context("nothing to redo")??;
                    restore(&root, &store, pre.id, vec![], false, false)?;
                }
                Outcome::Quit => {}
            }
        }
        Cmd::Gc { keep } => {
            let cfg = Config::load(&root, config::user_config_path().as_deref())?;
            // `--keep N` is a one-shot cap (newest N only). Without it, keep_turns ∪ keep_days.
            let (keep_n, keep_days) = match keep {
                Some(n) => (n, 0),
                None => (cfg.gc.keep_turns, cfg.gc.keep_days),
            };
            let _l = store.lock()?;
            let keep_ids = gc_keep_ids(&store, keep_n, keep_days)?;
            let r = store.gc(&keep_ids)?;
            println!(
                "removed {} snapshots, {} blobs ({} loose, {} packed); packs {} -> {}",
                r.manifests_removed,
                r.loose_removed + r.packed_removed,
                r.loose_removed,
                r.packed_removed,
                r.packs_before,
                r.packs_after
            );
        }
        Cmd::Hook { .. } => unreachable!(),
    }
    Ok(())
}

fn hook_agents(
    positional: Option<AgentArg>,
    flag: Option<AgentSel>,
    mutating: bool,
) -> Result<Vec<Agent>> {
    if let Some(sel) = flag {
        return Ok(match sel {
            AgentSel::All => Agent::ALL.to_vec(),
            AgentSel::Claude => vec![Agent::Claude],
            AgentSel::Codex => vec![Agent::Codex],
            AgentSel::Gemini => vec![Agent::Gemini],
        });
    }
    if let Some(a) = positional {
        return Ok(vec![a.into()]);
    }
    if mutating {
        return Ok(Agent::ALL.to_vec());
    }
    bail!("specify an agent (claude, codex, gemini) or pass --write/--remove")
}

fn cmd_hooks(
    root: &Path,
    positional: Option<AgentArg>,
    write: bool,
    remove: bool,
    agent_flag: Option<AgentSel>,
    scope: Scope,
    dry_run: bool,
) -> Result<()> {
    if write && remove {
        bail!("use either --write or --remove, not both");
    }
    if !write && !remove {
        if dry_run {
            bail!("--dry-run requires --write or --remove");
        }
        let agents = hook_agents(positional, agent_flag, false)?;
        for a in agents {
            println!("{}", unwreck_hooks::install_snippet(a));
        }
        return Ok(());
    }
    let agents = hook_agents(positional, agent_flag, true)?;
    let paths = InstallPaths::from_env(root.to_path_buf());
    for agent in agents {
        let plan = if write {
            install::plan_install(agent, scope, &paths)?
        } else {
            install::plan_remove(agent, scope, &paths)?
        };
        let rel = plan.path.display();
        if dry_run {
            if plan.changed {
                print!("{}", install::unified_diff(&plan));
            } else {
                println!("# {rel}: no change");
            }
            continue;
        }
        install::apply_plan(&plan)?;
        if plan.changed {
            let verb = if write { "wrote" } else { "updated" };
            if plan.original.is_empty() {
                println!("{verb} {rel}");
            } else {
                println!(
                    "{verb} {rel} (backup {})",
                    install::backup_path(&plan.path).display()
                );
            }
        } else {
            println!("{rel}: already up to date");
        }
        if write && agent == Agent::Codex && plan.changed {
            println!("note: Codex asks you to review and trust a new hook once before it runs.");
        }
    }
    Ok(())
}

fn project_root(explicit: Option<&Path>) -> Result<PathBuf> {
    let start = match explicit {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let start = start.canonicalize()?;
    let mut cur = start.as_path();
    loop {
        if cur.join(".git").exists() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return Ok(start),
        }
    }
}

/// Store lives OUTSIDE the worktree so `git clean -fdx` or `rm -rf .` can't take it out.
fn open_store(root: &Path) -> Result<Store> {
    let base = match std::env::var_os("UNWRECK_DIR") {
        Some(d) => PathBuf::from(d),
        None => directories::ProjectDirs::from("dev", "unwreck", "unwreck")
            .context("no home directory")?
            .data_dir()
            .to_path_buf(),
    };
    let key = blake3_hex(root.to_string_lossy().as_bytes());
    Ok(Store::open(base.join("projects").join(&key[..16]))?)
}

fn blake3_hex(b: &[u8]) -> String {
    Store::hash_bytes(b).to_hex()
}

fn snap(root: &Path, store: &Store, trigger: Trigger) -> Result<Manifest> {
    let _l = store.lock()?;
    Ok(unwreck_scan::snapshot(root, store, &ScanPolicy::default(), trigger)?.manifest)
}

/// Scan into a throwaway store just to compare against the disk (no history written).
fn scratch_scan(root: &Path) -> Result<Manifest> {
    let tmp = std::env::temp_dir().join(format!("unwreck-scratch-{}", std::process::id()));
    let s = Store::open(&tmp)?;
    let m = unwreck_scan::snapshot(
        root,
        &s,
        &ScanPolicy::default(),
        Trigger::Manual { label: None },
    )?
    .manifest;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(m)
}

fn diff(a: &Manifest, b: &Manifest) -> Vec<(char, String)> {
    let paths: BTreeSet<&str> = a
        .entries
        .iter()
        .chain(b.entries.iter())
        .map(|e| e.path.as_str())
        .collect();
    let (ad, bd): (BTreeSet<&String>, BTreeSet<&String>) =
        (a.empty_dirs.iter().collect(), b.empty_dirs.iter().collect());
    let dirs = ad.symmetric_difference(&bd).map(|d| {
        (
            if ad.contains(d) { 'D' } else { 'A' },
            format!("{d}/ (empty dir)"),
        )
    });
    paths
        .into_iter()
        .filter_map(|p| match (a.get(p), b.get(p)) {
            (Some(x), Some(y)) if x == y => None,
            (Some(_), Some(_)) => Some(('M', p.to_owned())),
            (Some(_), None) => Some(('D', p.to_owned())),
            (None, Some(_)) => Some(('A', p.to_owned())),
            (None, None) => None,
        })
        .chain(dirs)
        .collect()
}

fn ago(ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn timeline(store: &Store, limit: usize) -> Result<Vec<unwreck_tui::Row>> {
    let ids = store.list()?;
    let mut rows = Vec::new();
    let mut prev: Option<Manifest> = None;
    for id in ids {
        let m = store.read_manifest(id)?;
        let changed = prev.as_ref().map_or(m.entries.len(), |p| diff(p, &m).len());
        rows.push(unwreck_tui::Row {
            id,
            when: ago(m.created_unix_ms),
            label: m.trigger.label(),
            changed,
        });
        prev = Some(m);
    }
    let skip = rows.len().saturating_sub(limit);
    Ok(rows.into_iter().skip(skip).collect())
}

fn log(store: &Store, limit: usize) -> Result<()> {
    for r in timeline(store, limit)? {
        println!(
            "{}{:<5} {:>8}  {:>4} changed  {}",
            style::wrap("1", "#"),
            r.id,
            r.when,
            r.changed,
            r.label
        );
    }
    Ok(())
}

fn gc_keep_ids(store: &Store, keep_turns: usize, keep_days: u32) -> Result<Vec<u64>> {
    let ids = store.list()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let cutoff = now.saturating_sub(u64::from(keep_days) * 86_400_000);
    let mut keep = BTreeSet::new();
    for &id in ids.iter().rev().take(keep_turns) {
        keep.insert(id);
    }
    for &id in &ids {
        let m = store.read_manifest(id)?;
        if m.created_unix_ms >= cutoff {
            keep.insert(id);
        }
    }
    Ok(keep.into_iter().collect())
}

struct SnapDiffs<'a> {
    store: &'a Store,
    prev: std::collections::HashMap<u64, u64>,
}

impl DiffSource for SnapDiffs<'_> {
    fn files(&self, snapshot_id: u64) -> Vec<(char, String)> {
        let Ok(cur) = self.store.read_manifest(snapshot_id) else {
            return Vec::new();
        };
        match self
            .prev
            .get(&snapshot_id)
            .and_then(|p| self.store.read_manifest(*p).ok())
        {
            Some(p) => diff(&p, &cur),
            None => cur.entries.iter().map(|e| ('A', e.path.clone())).collect(),
        }
    }

    fn unified(&self, snapshot_id: u64, path: &str) -> String {
        let Ok(cur) = self.store.read_manifest(snapshot_id) else {
            return String::new();
        };
        let old = self
            .prev
            .get(&snapshot_id)
            .and_then(|p| self.store.read_manifest(*p).ok());
        match old {
            Some(p) => patch::unified(self.store, &p, &cur, path),
            None => patch::unified(
                self.store,
                &Manifest {
                    version: cur.version,
                    id: 0,
                    created_unix_ms: 0,
                    trigger: Trigger::Manual { label: None },
                    entries: vec![],
                    empty_dirs: vec![],
                },
                &cur,
                path,
            ),
        }
    }
}

/// Latest turn-boundary snapshot that differs from the current latest state.
fn last_turn_start(store: &Store) -> Result<u64> {
    let ids = store.list()?;
    for &id in ids.iter().rev() {
        let m = store.read_manifest(id)?;
        if matches!(m.trigger, Trigger::TurnStart { .. }) {
            return Ok(id);
        }
    }
    bail!("no turn-start snapshot yet (run an agent turn, or `unwreck diff <id>`)")
}

fn undo_target(store: &Store) -> Result<u64> {
    let ids = store.list()?;
    let latest = store.latest()?.context("no snapshots yet")?;
    for &id in ids.iter().rev() {
        let m = store.read_manifest(id)?;
        if m.trigger.is_turn_boundary() && !m.same_tree(&latest) {
            return Ok(id);
        }
    }
    bail!("no earlier turn to undo to")
}

fn restore(
    root: &Path,
    store: &Store,
    id: u64,
    only: Vec<String>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let target = store.read_manifest(id)?;
    let last = store.latest()?.context("no snapshots yet")?;
    let current = if dry_run {
        scratch_scan(root)?
    } else {
        snap(root, store, Trigger::PreRestore { restoring: id })?
    };
    let only_set: Option<BTreeSet<String>> = (!only.is_empty()).then(|| only.into_iter().collect());
    let plan = unwreck_restore::plan(&target, &last, &current, only_set.as_ref());
    for a in &plan.actions {
        match a {
            Action::Write(e) => println!("  restore {}", e.path),
            Action::Delete(p) => println!("  delete  {p}"),
            Action::Conflict { path, .. } if force => println!("  FORCE   {path}"),
            Action::Conflict { path, .. } => {
                println!("  SKIP    {path}  (changed after last snapshot; --force to overwrite)")
            }
            Action::Unrecoverable(p) => {
                println!("  MISSING {p}  (was over the size cap; contents not stored)")
            }
            Action::MkDir(d) => println!("  mkdir   {d}/"),
            Action::RmDir(d) => println!("  rmdir   {d}/  (empty dir not in target)"),
        }
    }
    if dry_run {
        for d in plan.preview_pruned_dirs() {
            println!("  rmdir   {d}/  (would remove if left empty)");
        }
        println!("dry run: nothing changed");
        return Ok(());
    }
    let applied = {
        let _l = store.lock()?;
        unwreck_restore::apply(root, store, &plan, force)?
    };
    for d in &applied.removed_dirs {
        println!("  rmdir   {d}/  (emptied by this restore)");
    }
    let n = applied.changed;
    let post = snap(root, store, Trigger::PostRestore { restored: id })?;
    println!(
        "restored {n} paths to #{id}; your previous state is #{} (`unwreck redo`)",
        current.id
    );
    let _ = post;
    Ok(())
}

fn hook(root: Option<&Path>, agent: Agent) -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let p = unwreck_hooks::parse(&raw)?;
    let from_payload = p.cwd.as_deref().map(Path::new).filter(|p| p.is_dir());
    let root = project_root(root.or(from_payload))?;
    let store = open_store(&root)?;
    let a = agent.name().to_owned();
    let trigger = match p.event() {
        HookEvent::TurnStart { prompt } => Trigger::TurnStart {
            agent: a,
            session_id: p.session_id.clone(),
            turn_id: p.turn_key(),
            prompt,
        },
        HookEvent::PreTool { tool, detail } => Trigger::PreTool {
            agent: a,
            session_id: p.session_id.clone(),
            turn_id: p.turn_key(),
            tool,
            detail,
        },
        HookEvent::TurnEnd => Trigger::TurnEnd {
            agent: a,
            session_id: p.session_id.clone(),
            turn_id: p.turn_key(),
        },
        HookEvent::Ignore => return Ok(()),
    };
    snap(&root, &store, trigger)?;
    Ok(())
}

fn run(root: &Path, store: &Store, quiet_ms: u64, command: &[String]) -> Result<()> {
    let agent = Path::new(&command[0])
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());
    snap(
        root,
        store,
        Trigger::Quiescent {
            agent: agent.clone(),
        },
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = {
        let (root, store_dir) = (root.to_path_buf(), store.root().to_path_buf());
        let agent = agent.clone();
        std::thread::spawn(move || -> Result<()> {
            let store = Store::open(store_dir)?;
            watch_loop(&root, &store, quiet_ms, agent, rx)
        })
    };
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| format!("spawning {}", command[0]))?;
    let _ = tx.send(());
    let _ = watcher.join();
    snap(root, store, Trigger::Quiescent { agent })?;
    std::process::exit(status.code().unwrap_or(1));
}

fn watch_loop(
    root: &Path,
    store: &Store,
    quiet_ms: u64,
    agent: Option<String>,
    stop: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    let cfg = unwreck_watch::WatchConfig {
        root: root.to_path_buf(),
        quiet: Duration::from_millis(quiet_ms),
        ignore_prefixes: vec![store.root().to_path_buf()],
    };
    unwreck_watch::run(cfg, stop, |_n| {
        if let Err(e) = snap(
            root,
            store,
            Trigger::Quiescent {
                agent: agent.clone(),
            },
        ) {
            eprintln!("unwreck: snapshot failed: {e:#}");
        }
    })?;
    Ok(())
}
