//! Merge/remove unwreck hook entries in agent config files.
//!
//! Doc URLs (checked 2026-10-18, re-checked this change):
//! * Claude Code: <https://code.claude.com/docs/en/hooks>
//! * Codex: <https://developers.openai.com/codex/hooks> (also <https://learn.chatgpt.com/docs/hooks>)
//! * Gemini CLI: <https://geminicli.com/docs/hooks/reference/>
use crate::Agent;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

/// User-wide vs project-local config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `~/.claude/settings.json`, `~/.codex/hooks.json`, `~/.gemini/settings.json`.
    User,
    /// `.claude/settings.json`, `.codex/hooks.json`, `.gemini/settings.json` in the project.
    Project,
}

/// Failures reading or merging hook config.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// Filesystem error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Config is not JSON we can merge (file left untouched).
    #[error("{path}: {source}")]
    Json {
        /// Config path.
        path: String,
        /// Parse error.
        source: serde_json::Error,
    },
    /// JSON is the wrong shape (not an object, or `hooks` isn't a map of arrays).
    #[error("{0}")]
    Shape(String),
}

/// One config file we would write.
#[derive(Debug, Clone)]
pub struct FilePlan {
    /// Absolute path of the agent config.
    pub path: PathBuf,
    /// Current bytes (empty if the file does not exist).
    pub original: Vec<u8>,
    /// Bytes we would write.
    pub next: Vec<u8>,
    /// True if `next` differs from `original`.
    pub changed: bool,
}

/// Where to look for configs. `home` is injectable so tests don't touch `$HOME`.
#[derive(Debug, Clone)]
pub struct InstallPaths {
    /// User home directory.
    pub home: PathBuf,
    /// Project root (git worktree or cwd).
    pub project: PathBuf,
}

impl InstallPaths {
    /// `HOME` / `USERPROFILE`, else `/`.
    pub fn from_env(project: PathBuf) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self { home, project }
    }
}

/// Backup sibling written before a mutating install/remove.
pub fn backup_path(config: &Path) -> PathBuf {
    let mut name = config.file_name().unwrap_or_default().to_os_string();
    name.push(".unwreck.bak");
    config.with_file_name(name)
}

/// Config path for `agent` at `scope`.
///
/// Claude: <https://code.claude.com/docs/en/hooks> — `~/.claude/settings.json` or `.claude/settings.json`.
/// Codex: <https://developers.openai.com/codex/hooks> — `~/.codex/hooks.json` or `<repo>/.codex/hooks.json`.
///   UNVERIFIED this change: whether a given Codex build prefers `hooks.json` vs inline `[hooks]` in
///   `config.toml` when both exist (docs say it merges and warns; we only write `hooks.json`).
/// Gemini: <https://geminicli.com/docs/hooks/reference/> — `~/.gemini/settings.json` or `.gemini/settings.json`.
pub fn config_path(agent: Agent, scope: Scope, paths: &InstallPaths) -> PathBuf {
    match (agent, scope) {
        (Agent::Claude, Scope::User) => paths.home.join(".claude/settings.json"),
        (Agent::Claude, Scope::Project) => paths.project.join(".claude/settings.json"),
        (Agent::Codex, Scope::User) => paths.home.join(".codex/hooks.json"),
        (Agent::Codex, Scope::Project) => paths.project.join(".codex/hooks.json"),
        (Agent::Gemini, Scope::User) => paths.home.join(".gemini/settings.json"),
        (Agent::Gemini, Scope::Project) => paths.project.join(".gemini/settings.json"),
    }
}

/// Command string we register (`unwreck hook claude`, …).
pub fn hook_command(agent: Agent) -> String {
    format!("unwreck hook {}", agent.name())
}

/// Events and optional tool matchers. See module-level doc URLs.
pub fn event_specs(agent: Agent) -> &'static [(&'static str, Option<&'static str>)] {
    match agent {
        // Matcher includes MultiEdit|NotebookEdit from the Claude hooks reference (not only Write|Edit).
        Agent::Claude => &[
            ("UserPromptSubmit", None),
            ("PreToolUse", Some("Bash|Write|Edit|MultiEdit|NotebookEdit")),
            ("Stop", None),
        ],
        Agent::Codex => &[
            ("UserPromptSubmit", None),
            // Docs (2026-10-18): shell + unified exec (`exec_command`) match as `Bash`;
            // apply_patch also as Edit|Write. A live Codex session fired PreToolUse for
            // apply_patch but not for `rm -rf` with matcher `Bash|apply_patch`, so the
            // actual shell tool_name is UNVERIFIED. `"*"` matches every PreToolUse
            // (documented: `"*"`, `""`, or omit matcher).
            ("PreToolUse", Some("*")),
            ("Stop", None),
        ],
        // UNVERIFIED: Gemini docs do not say whether omitting `matcher` on BeforeAgent/AfterAgent
        // is equivalent to `"*"`; we omit it (same as Claude lifecycle events).
        Agent::Gemini => &[
            ("BeforeAgent", None),
            ("BeforeTool", Some("run_shell_command|write_file|replace")),
            ("AfterAgent", None),
        ],
    }
}

fn command_is_ours(cmd: &str, agent: Agent) -> bool {
    let want = agent.name();
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    parts.windows(3).any(|w| {
        Path::new(w[0]).file_name().is_some_and(|f| f == "unwreck")
            && w[1] == "hook"
            && w[2] == want
    })
}

fn parse_root(bytes: &[u8], path: &Path) -> Result<Value, InstallError> {
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(bytes).map_err(|source| InstallError::Json {
        path: path.display().to_string(),
        source,
    })
}

fn as_object_mut<'a>(
    v: &'a mut Value,
    ctx: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, InstallError> {
    v.as_object_mut()
        .ok_or_else(|| InstallError::Shape(format!("{ctx} must be a JSON object")))
}

fn encode(v: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string_pretty(v).expect("Value is serializable");
    s.push('\n');
    s.into_bytes()
}

fn merge_into(root: &mut Value, agent: Agent) -> Result<bool, InstallError> {
    let mut changed = false;
    let cmd = hook_command(agent);
    let obj = as_object_mut(root, "config")?;
    let hooks_val = obj.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let hooks = as_object_mut(hooks_val, "hooks")?;
    for &(event, matcher) in event_specs(agent) {
        let arr_val = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        let arr = arr_val
            .as_array_mut()
            .ok_or_else(|| InstallError::Shape(format!("hooks.{event} must be an array")))?;
        if let Some(g) = arr.iter_mut().find(|g| group_has_ours(g, agent)) {
            if sync_matcher(g, matcher) {
                changed = true;
            }
            continue;
        }
        let mut group = json!({"hooks": [{"type": "command", "command": cmd}]});
        if let Some(m) = matcher {
            group
                .as_object_mut()
                .unwrap()
                .insert("matcher".into(), json!(m));
        }
        arr.push(group);
        changed = true;
    }
    Ok(changed)
}

/// Set or clear `matcher` on an existing group. Returns true if the JSON changed.
fn sync_matcher(group: &mut Value, matcher: Option<&str>) -> bool {
    let Some(obj) = group.as_object_mut() else {
        return false;
    };
    match matcher {
        Some(m) if obj.get("matcher").and_then(|v| v.as_str()) != Some(m) => {
            obj.insert("matcher".into(), json!(m));
            true
        }
        None if obj.contains_key("matcher") => {
            obj.remove("matcher");
            true
        }
        _ => false,
    }
}

fn group_has_ours(group: &Value, agent: Agent) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| command_is_ours(c, agent))
            })
        })
}

fn unmerge(root: &mut Value, agent: Agent) -> Result<bool, InstallError> {
    let obj = as_object_mut(root, "config")?;
    let Some(hooks_val) = obj.get("hooks") else {
        return Ok(false);
    };
    let mut hooks = hooks_val
        .as_object()
        .ok_or_else(|| InstallError::Shape("hooks must be a JSON object".into()))?
        .clone();
    let mut changed = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let arr = match hooks.get(&event).and_then(|v| v.as_array()) {
            Some(a) => a.clone(),
            None => {
                return Err(InstallError::Shape(format!(
                    "hooks.{event} must be an array"
                )));
            }
        };
        let mut new_groups = Vec::new();
        for g in arr {
            let Some(hs) = g.get("hooks").and_then(|h| h.as_array()) else {
                new_groups.push(g);
                continue;
            };
            let kept: Vec<Value> = hs
                .iter()
                .filter(|h| {
                    !h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| command_is_ours(c, agent))
                })
                .cloned()
                .collect();
            if kept.len() != hs.len() {
                changed = true;
            }
            if kept.is_empty() {
                continue;
            }
            let mut g = g.clone();
            if let Some(o) = g.as_object_mut() {
                o.insert("hooks".into(), Value::Array(kept));
            }
            new_groups.push(g);
        }
        if new_groups.is_empty() {
            hooks.remove(&event);
            changed = true;
        } else {
            hooks.insert(event, Value::Array(new_groups));
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    } else {
        obj.insert("hooks".into(), Value::Object(hooks));
    }
    Ok(changed)
}

fn read_original(path: &Path) -> Result<Vec<u8>, InstallError> {
    match fs::read(path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Merge unwreck's hooks into the config (idempotent).
pub fn plan_install(
    agent: Agent,
    scope: Scope,
    paths: &InstallPaths,
) -> Result<FilePlan, InstallError> {
    let path = config_path(agent, scope, paths);
    let original = read_original(&path)?;
    let mut value = parse_root(&original, &path)?;
    let merged = merge_into(&mut value, agent)?;
    if !merged {
        return Ok(FilePlan {
            path,
            original: original.clone(),
            next: original,
            changed: false,
        });
    }
    Ok(FilePlan {
        path,
        original,
        next: encode(&value),
        changed: true,
    })
}

/// Strip unwreck's hooks. If a `.unwreck.bak` parses equal to the result, restore those bytes.
pub fn plan_remove(
    agent: Agent,
    scope: Scope,
    paths: &InstallPaths,
) -> Result<FilePlan, InstallError> {
    let path = config_path(agent, scope, paths);
    let original = read_original(&path)?;
    if original.is_empty() {
        return Ok(FilePlan {
            path,
            original,
            next: Vec::new(),
            changed: false,
        });
    }
    let mut value = parse_root(&original, &path)?;
    let stripped = unmerge(&mut value, agent)?;
    if !stripped {
        return Ok(FilePlan {
            path,
            original: original.clone(),
            next: original,
            changed: false,
        });
    }
    let mut next = if value.as_object().is_some_and(|o| o.is_empty()) {
        Vec::new()
    } else {
        encode(&value)
    };
    if let Ok(bak_bytes) = fs::read(backup_path(&path))
        && let Ok(bak_val) = serde_json::from_slice::<Value>(&bak_bytes)
        && bak_val == value
    {
        next = bak_bytes;
    }
    Ok(FilePlan {
        path,
        original,
        next,
        changed: true,
    })
}

/// Write `plan.next` (or delete the file if empty). Backs up when changing an existing file.
pub fn apply_plan(plan: &FilePlan) -> Result<(), InstallError> {
    if !plan.changed {
        return Ok(());
    }
    if !plan.original.is_empty() {
        if let Some(parent) = plan.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(backup_path(&plan.path), &plan.original)?;
    }
    if plan.next.is_empty() {
        match fs::remove_file(&plan.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(());
    }
    if let Some(parent) = plan.path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = plan.path.with_extension("json.unwreck-tmp");
    fs::write(&tmp, &plan.next)?;
    fs::rename(&tmp, &plan.path)?;
    Ok(())
}

/// Whether this config already contains `unwreck hook AGENT`.
pub fn installed_in(bytes: &[u8], agent: Agent) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    v.get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(|hooks| {
            hooks.values().any(|arr| {
                arr.as_array()
                    .is_some_and(|groups| groups.iter().any(|g| group_has_ours(g, agent)))
            })
        })
}

/// User and/or project configs that currently list our hook.
pub fn install_status(agent: Agent, paths: &InstallPaths) -> (bool, bool) {
    let user = fs::read(config_path(agent, Scope::User, paths))
        .ok()
        .is_some_and(|b| installed_in(&b, agent));
    let project = fs::read(config_path(agent, Scope::Project, paths))
        .ok()
        .is_some_and(|b| installed_in(&b, agent));
    (user, project)
}

/// Unified diff of a planned rewrite (empty if unchanged).
pub fn unified_diff(plan: &FilePlan) -> String {
    if !plan.changed {
        return String::new();
    }
    let rel = plan.path.display().to_string();
    let old = String::from_utf8_lossy(&plan.original);
    let new = String::from_utf8_lossy(&plan.next);
    let ol: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let nl: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.lines().collect()
    };
    let mut out = format!(
        "--- a/{rel}\n+++ b/{rel}\n@@ -1,{} +1,{} @@\n",
        ol.len(),
        nl.len()
    );
    for l in ol {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in nl {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, tempfile::TempDir, InstallPaths) {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let p = InstallPaths {
            home: home.path().to_path_buf(),
            project: proj.path().to_path_buf(),
        };
        (home, proj, p)
    }

    fn user_cfg(p: &InstallPaths, agent: Agent) -> PathBuf {
        config_path(agent, Scope::User, p)
    }

    #[test]
    fn install_twice_then_remove_restores_original_bytes() {
        let (_h, _j, p) = paths();
        let path = user_cfg(&p, Agent::Claude);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"{\n  \"env\": { \"FOO\": \"1\" },\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"echo hi\" }\n        ]\n      }\n    ]\n  }\n}\n";
        fs::write(&path, original).unwrap();
        let a = plan_install(Agent::Claude, Scope::User, &p).unwrap();
        assert!(a.changed);
        apply_plan(&a).unwrap();
        let b = plan_install(Agent::Claude, Scope::User, &p).unwrap();
        assert!(!b.changed, "second install must be a no-op");
        assert_eq!(fs::read(&path).unwrap(), a.next);
        let r = plan_remove(Agent::Claude, Scope::User, &p).unwrap();
        apply_plan(&r).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn keeps_existing_user_hooks() {
        let (_h, _j, p) = paths();
        let path = user_cfg(&p, Agent::Codex);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        )
        .unwrap();
        apply_plan(&plan_install(Agent::Codex, Scope::User, &p).unwrap()).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert!(stop.iter().any(|g| group_has_ours(g, Agent::Codex)));
        assert!(stop.iter().any(|g| {
            g["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["command"] == "echo mine")
        }));
        apply_plan(&plan_remove(Agent::Codex, Scope::User, &p).unwrap()).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "echo mine");
        assert!(!installed_in(&fs::read(&path).unwrap(), Agent::Codex));
    }

    #[test]
    fn malformed_config_is_not_overwritten() {
        let (_h, _j, p) = paths();
        let path = user_cfg(&p, Agent::Gemini);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let junk = b"not json {{{";
        fs::write(&path, junk).unwrap();
        let err = plan_install(Agent::Gemini, Scope::User, &p).unwrap_err();
        assert!(matches!(err, InstallError::Json { .. }));
        assert_eq!(fs::read(&path).unwrap(), junk);
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn missing_file_install_remove_deletes_it() {
        let (_h, _j, p) = paths();
        let path = user_cfg(&p, Agent::Claude);
        apply_plan(&plan_install(Agent::Claude, Scope::User, &p).unwrap()).unwrap();
        assert!(path.exists());
        apply_plan(&plan_remove(Agent::Claude, Scope::User, &p).unwrap()).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn reinstall_upgrades_codex_pretool_matcher_then_is_idempotent() {
        let (_h, _j, p) = paths();
        let path = user_cfg(&p, Agent::Codex);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash|apply_patch","hooks":[{"type":"command","command":"unwreck hook codex"}]}]}}"#,
        )
        .unwrap();
        let a = plan_install(Agent::Codex, Scope::User, &p).unwrap();
        assert!(a.changed);
        apply_plan(&a).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
        let b = plan_install(Agent::Codex, Scope::User, &p).unwrap();
        assert!(!b.changed);
    }

    #[test]
    fn full_path_command_still_counts_as_ours() {
        assert!(command_is_ours(
            "/usr/local/bin/unwreck hook claude",
            Agent::Claude
        ));
        assert!(!command_is_ours("unwreck hook gemini", Agent::Claude));
    }
}
