//! `unwreck doctor`: store, hooks, agent CLIs, git. Exit 1 only on fail checks.
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use unwreck_core::EntryKind;
use unwreck_hooks::Agent;
use unwreck_hooks::install::{InstallPaths, install_status};
use unwreck_scan::ScanPolicy;
use unwreck_store::Store;

/// One doctor line.
#[derive(Debug, Clone)]
pub struct Check {
    /// Stable id (`store`, `hooks.claude`, …).
    pub id: String,
    /// `ok` / `warn` / `fail`.
    pub level: Level,
    /// Human message (without the glyph).
    pub message: String,
    /// What to do, if anything.
    pub hint: Option<String>,
}

/// Severity. Process exits 1 iff any check is [`Level::Fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// ✓
    Ok,
    /// !
    Warn,
    /// ✗
    Fail,
}

impl Level {
    fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Warn => "!",
            Self::Fail => "✗",
        }
    }
    fn json(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// Full doctor report.
#[derive(Debug, Clone)]
pub struct Report {
    /// Ordered checks.
    pub checks: Vec<Check>,
}

impl Report {
    /// True if any check is a hard failure.
    pub fn has_fail(&self) -> bool {
        self.checks.iter().any(|c| c.level == Level::Fail)
    }

    /// Human lines with glyphs and hints.
    pub fn print(&self) {
        for c in &self.checks {
            println!("{} {}", c.level.glyph(), c.message);
            if let Some(h) = &c.hint {
                println!("    fix: {h}");
            }
        }
    }

    /// Machine-readable report.
    pub fn to_json(&self) -> Value {
        json!({
            "ok": !self.has_fail(),
            "checks": self.checks.iter().map(|c| json!({
                "id": c.id,
                "level": c.level.json(),
                "message": c.message,
                "hint": c.hint,
            })).collect::<Vec<_>>(),
        })
    }
}

fn ok(id: &str, message: String) -> Check {
    Check {
        id: id.into(),
        level: Level::Ok,
        message,
        hint: None,
    }
}
fn warn(id: &str, message: String, hint: &str) -> Check {
    Check {
        id: id.into(),
        level: Level::Warn,
        message,
        hint: Some(hint.into()),
    }
}
fn fail(id: &str, message: String, hint: &str) -> Check {
    Check {
        id: id.into(),
        level: Level::Fail,
        message,
        hint: Some(hint.into()),
    }
}

fn human_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let x = n as f64;
    if x < K {
        format!("{n} B")
    } else if x < K * K {
        format!("{:.1} KiB", x / K)
    } else if x < K * K * K {
        format!("{:.1} MiB", x / (K * K))
    } else {
        format!("{:.1} GiB", x / (K * K * K))
    }
}

fn dir_du(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_dir() {
        fs::read_dir(path)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| dir_du(&e.path()))
            .sum()
    } else {
        meta.len()
    }
}

/// Probe `bin --version` (first line of stdout, else stderr).
pub fn probe_cli(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if !line.is_empty() {
        return Some(line.to_string());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        Some("(unknown version)".into())
    } else {
        Some(line.to_string())
    }
}

/// Probe the three agent binaries on PATH.
pub fn probe_agents() -> Vec<(Agent, Option<String>)> {
    Agent::ALL
        .into_iter()
        .map(|a| (a, probe_cli(a.name())))
        .collect()
}

/// Run every doctor check. Does not print or exit.
pub fn check(
    root: &Path,
    store: &Store,
    hook_paths: &InstallPaths,
    clis: &[(Agent, Option<String>)],
) -> Report {
    let mut checks = Vec::new();

    let loc = store.root().display().to_string();
    if store.root().is_dir() {
        checks.push(ok("store", format!("store {loc}")));
    } else {
        checks.push(fail(
            "store",
            format!("store missing: {loc}"),
            "run `unwreck snap` once to create it",
        ));
        return Report { checks };
    }

    for w in store.permission_warnings() {
        checks.push(warn(
            "store.perms",
            w,
            "chmod 0700 the store directory and 0600 its files",
        ));
    }
    if !checks.iter().any(|c| c.id == "store.perms") {
        checks.push(ok("store.perms", "store permissions 0700/0600".into()));
    }

    let du = store.du_bytes();
    match store.stats() {
        Ok((packs, pack_bytes, loose)) => checks.push(ok(
            "store.du",
            format!(
                "disk {} ({} packs, {} pack bytes, {loose} loose objects)",
                human_bytes(du),
                packs,
                human_bytes(pack_bytes)
            ),
        )),
        Err(e) => checks.push(fail(
            "store.du",
            format!("cannot measure store: {e}"),
            "check that the store directory is readable",
        )),
    }

    match store.list() {
        Err(e) => checks.push(fail(
            "store.read",
            format!("store not readable: {e}"),
            "check ownership and that UNWRECK_DIR points at this store",
        )),
        Ok(_) => match store.latest() {
            Err(e) => checks.push(fail(
                "store.latest",
                format!("latest snapshot could not be read: {e}"),
                "the newest manifests/*.json.zst may be corrupt; restore from an older id or `unwreck gc`",
            )),
            Ok(None) => checks.push(warn(
                "store.latest",
                "no snapshots yet".into(),
                "run `unwreck snap` or use an agent with hooks installed",
            )),
            Ok(Some(m)) => {
                let mut bad = None;
                for e in &m.entries {
                    if let EntryKind::File { hash, .. } = &e.kind
                        && let Err(err) = store.get(hash)
                    {
                        bad = Some(format!("{}: {err}", e.path));
                        break;
                    }
                }
                match bad {
                    Some(b) => checks.push(fail(
                        "store.latest",
                        format!("snapshot #{} is invalid ({b})", m.id),
                        "try an older snapshot with `unwreck log`; `gc` cannot repair missing blobs",
                    )),
                    None => checks.push(ok(
                        "store.latest",
                        format!("latest snapshot #{} valid ({} files)", m.id, m.entries.len()),
                    )),
                }
            }
        },
    }

    for agent in Agent::ALL {
        let (user, project) = install_status(agent, hook_paths);
        let id = format!("hooks.{}", agent.name());
        match (user, project) {
            (false, false) => checks.push(warn(
                &id,
                format!("{} hooks not installed", agent.name()),
                &format!(
                    "unwreck hooks --write --agent {} --scope project",
                    agent.name()
                ),
            )),
            (u, p) => {
                let where_ = match (u, p) {
                    (true, true) => "user and project",
                    (true, false) => "user",
                    (false, true) => "project",
                    _ => unreachable!(),
                };
                checks.push(ok(
                    &id,
                    format!("{} hooks installed ({where_})", agent.name()),
                ));
            }
        }
    }

    for &(agent, ref ver) in clis {
        let id = format!("cli.{}", agent.name());
        match ver {
            Some(v) => checks.push(ok(&id, format!("{} on PATH: {v}", agent.name()))),
            None => checks.push(warn(
                &id,
                format!("{} CLI not on PATH", agent.name()),
                &format!("install {} if you use that agent", agent.name()),
            )),
        }
    }

    if root.join(".git").exists() {
        checks.push(ok("git", format!("git repo {}", root.display())));
    } else {
        checks.push(warn(
            "git",
            "not a git repo (unwreck still works)".into(),
            "git init if you want ignore files honored the usual way",
        ));
    }

    const SIZEABLE: u64 = 8 * 1024 * 1024;
    let policy = ScanPolicy::default();
    let mut heavy = Vec::new();
    for name in &policy.heavy_dirs {
        let p = root.join(name);
        if p.is_dir() {
            let n = dir_du(&p);
            if n >= SIZEABLE {
                heavy.push(format!("{name}/ ({})", human_bytes(n)));
            }
        }
    }
    if heavy.is_empty() {
        checks.push(ok(
            "heavy",
            "no sizeable skipped folders (node_modules, target, …)".into(),
        ));
    } else {
        checks.push(warn(
            "heavy",
            format!("sizeable ignored/skipped folders: {}", heavy.join(", ")),
            "unwreck skips these by name; nothing to do unless you meant to snapshot them",
        ));
    }

    Report { checks }
}

/// Print the report; caller should exit 1 on [`Report::has_fail`].
pub fn run(root: &Path, store: &Store, json: bool) -> Result<Report> {
    let paths = InstallPaths::from_env(root.to_path_buf());
    let clis = probe_agents();
    let report = check(root, store, &paths, &clis);
    if json {
        println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    } else {
        report.print();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use unwreck_core::Trigger;
    use unwreck_scan::snapshot;

    fn env_store() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Store,
        std::path::PathBuf,
    ) {
        let home = tempdir().unwrap();
        let proj = tempdir().unwrap();
        let root = proj.path().to_path_buf();
        fs::write(root.join("a.rs"), b"fn a(){}").unwrap();
        let store = Store::open(home.path().join("store")).unwrap();
        (home, proj, store, root)
    }

    #[test]
    fn healthy_store_has_no_fail() {
        let (home, _p, store, root) = env_store();
        let _l = store.lock().unwrap();
        snapshot(
            &root,
            &store,
            &ScanPolicy::default(),
            Trigger::Manual {
                label: Some("t".into()),
            },
        )
        .unwrap();
        drop(_l);
        let paths = InstallPaths {
            home: home.path().to_path_buf(),
            project: root.clone(),
        };
        let clis = vec![
            (Agent::Claude, Some("1.0".into())),
            (Agent::Codex, None),
            (Agent::Gemini, None),
        ];
        let r = check(&root, &store, &paths, &clis);
        assert!(
            !r.has_fail(),
            "{:?}",
            r.checks
                .iter()
                .map(|c| (&c.id, &c.message))
                .collect::<Vec<_>>()
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.id == "store.latest" && c.level == Level::Ok)
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.id == "cli.claude" && c.level == Level::Ok)
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.id == "cli.codex" && c.level == Level::Warn)
        );
        assert!(r.to_json()["ok"].as_bool() == Some(true));
    }

    #[test]
    fn broken_latest_manifest_is_fail() {
        let (home, _p, store, root) = env_store();
        let _l = store.lock().unwrap();
        let m = snapshot(
            &root,
            &store,
            &ScanPolicy::default(),
            Trigger::Manual { label: None },
        )
        .unwrap();
        drop(_l);
        let path = store
            .root()
            .join("manifests")
            .join(format!("{:016}.json.zst", m.manifest.id));
        fs::write(&path, b"not a zstd manifest").unwrap();
        let paths = InstallPaths {
            home: home.path().to_path_buf(),
            project: root.clone(),
        };
        let r = check(&root, &store, &paths, &[]);
        assert!(r.has_fail());
        assert!(
            r.checks
                .iter()
                .any(|c| c.id == "store.latest" && c.level == Level::Fail)
        );
        assert_eq!(r.to_json()["ok"], false);
    }
}
