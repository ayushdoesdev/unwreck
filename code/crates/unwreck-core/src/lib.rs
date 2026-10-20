//! Shared types for unwreck: content hashes, snapshot manifests, triggers, config.
#![warn(missing_docs)]

pub mod config;

use serde::{Deserialize, Serialize};
use std::fmt;

/// BLAKE3 digest of a file's raw (uncompressed) bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash(
    /// Raw 32-byte digest.
    #[serde(with = "hex32")]
    pub [u8; 32],
);

impl ContentHash {
    /// Lowercase hex encoding (64 characters).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", &self.to_hex()[..12])
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        if s.len() != 64 {
            return Err(D::Error::custom("bad hash length"));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(chunk).map_err(D::Error::custom)?;
            out[i] = u8::from_str_radix(hex, 16).map_err(D::Error::custom)?;
        }
        Ok(out)
    }
}

/// Repo-relative path, always '/'-separated, stored as UTF-8.
/// Non-UTF-8 paths are skipped with a warning in the MVP (see PLAN.md, "Paths").
pub type RelPath = String;

/// What a snapshot recorded at a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    /// Regular file captured in the object store.
    File {
        /// BLAKE3 of the file bytes.
        hash: ContentHash,
        /// Size in bytes.
        size: u64,
        /// Unix executable bit (any of `0o111`).
        executable: bool,
    },
    /// Symlink: we store the target string, never follow it.
    Symlink {
        /// Link target as stored (not resolved).
        target: String,
    },
    /// File seen but not captured (over size cap). Restore can only warn about it.
    Oversize {
        /// Size in bytes as observed on disk.
        size: u64,
    },
}

/// One path in a snapshot: a file, symlink, or oversize placeholder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Repo-relative path (`/`-separated).
    pub path: RelPath,
    /// What lives at `path`.
    #[serde(flatten)]
    pub kind: EntryKind,
}

/// What caused a snapshot. Drives `unwreck log` labels and turn grouping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "trigger", rename_all = "snake_case")]
pub enum Trigger {
    /// User submitted a prompt (`UserPromptSubmit` / `BeforeAgent`).
    TurnStart {
        /// Agent name (`claude`, `codex`, `gemini`, …).
        agent: String,
        /// Vendor session id, if the payload had one.
        session_id: Option<String>,
        /// Codex `turn_id` or Claude `prompt_id`.
        turn_id: Option<String>,
        /// Prompt text, truncated by the logger not here.
        prompt: Option<String>,
    },
    /// Immediately before a mutating tool (`PreToolUse` / `BeforeTool`).
    PreTool {
        /// Agent name.
        agent: String,
        /// Vendor session id, if present.
        session_id: Option<String>,
        /// Turn key, if present.
        turn_id: Option<String>,
        /// Tool name (`Bash`, `apply_patch`, …).
        tool: String,
        /// Command string or file path from the tool input, if any.
        detail: Option<String>,
    },
    /// The agent finished a turn (`Stop` / `AfterAgent`).
    TurnEnd {
        /// Agent name.
        agent: String,
        /// Vendor session id, if present.
        session_id: Option<String>,
        /// Turn key, if present.
        turn_id: Option<String>,
    },
    /// Filesystem went quiet under `unwreck run` / `watch`.
    Quiescent {
        /// Child process name, if this came from `run`.
        agent: Option<String>,
    },
    /// `unwreck snap` (always written, even if the tree is unchanged; gc pin).
    Manual {
        /// Optional `-m` message.
        label: Option<String>,
    },
    /// Snapshot of disk taken immediately before a restore (the redo point).
    PreRestore {
        /// Snapshot id about to be restored.
        restoring: u64,
    },
    /// Snapshot of disk taken immediately after a restore.
    PostRestore {
        /// Snapshot id that was restored.
        restored: u64,
    },
}

/// Manifest format version written by this build.
/// * 0 (field absent): original skeleton format, no directory tracking.
/// * 2: adds `empty_dirs`. Readers treat a missing field as 0 and an absent list as empty.
pub const MANIFEST_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// One snapshot of a worktree: identity, trigger, and the tree listing.
pub struct Manifest {
    /// Manifest format version. Missing in old files, read as 0.
    #[serde(default)]
    pub version: u32,
    /// Monotonic snapshot id, starting at 1 in a store.
    pub id: u64,
    /// Unix epoch milliseconds when this manifest was written.
    pub created_unix_ms: u64,
    /// Why this snapshot was taken.
    pub trigger: Trigger,
    /// Sorted by path for binary search and stable diffs.
    pub entries: Vec<Entry>,
    /// Directories that had no children at all on disk when the snapshot was taken
    /// (sorted). Non-empty directories are implied by the paths in `entries`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub empty_dirs: Vec<RelPath>,
}

impl Manifest {
    /// True if this manifest records empty directories (format version >= 2).
    /// Older manifests say nothing about directories, so restore must not prune
    /// directories on their behalf.
    pub fn tracks_dirs(&self) -> bool {
        self.version >= 2
    }

    /// Every directory that existed in this snapshot: ancestors of every entry plus
    /// the recorded empty directories. The root ("") is not included.
    pub fn dirs(&self) -> std::collections::BTreeSet<RelPath> {
        let mut out = std::collections::BTreeSet::new();
        for e in &self.entries {
            let mut p = e.path.as_str();
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                if !out.insert(p.to_owned()) {
                    break; // ancestors of p are already in the set
                }
            }
        }
        for d in &self.empty_dirs {
            let mut p = d.as_str();
            out.insert(p.to_owned());
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                out.insert(p.to_owned());
            }
        }
        out
    }

    /// Same file entries and the same empty directories.
    pub fn same_tree(&self, other: &Manifest) -> bool {
        self.entries == other.entries && self.empty_dirs == other.empty_dirs
    }

    /// Binary search for `path` in the sorted `entries` list.
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Content-addressed identity of the tree (entries + empty dirs), independent of id/trigger.
    pub fn tree_fingerprint(&self) -> ContentHash {
        tree_fingerprint(&self.entries, &self.empty_dirs)
    }
}

/// BLAKE3 of sorted entries and empty dirs. Used to skip writing an identical snapshot.
pub fn tree_fingerprint(entries: &[Entry], empty_dirs: &[RelPath]) -> ContentHash {
    let mut h = blake3::Hasher::new();
    for e in entries {
        h.update(e.path.as_bytes());
        h.update(&[0]);
        match &e.kind {
            EntryKind::File {
                hash,
                size,
                executable,
            } => {
                h.update(&[1]);
                h.update(&hash.0);
                h.update(&size.to_le_bytes());
                h.update(&[u8::from(*executable)]);
            }
            EntryKind::Symlink { target } => {
                h.update(&[2]);
                h.update(target.as_bytes());
            }
            EntryKind::Oversize { size } => {
                h.update(&[3]);
                h.update(&size.to_le_bytes());
            }
        }
        h.update(&[0xff]);
    }
    h.update(b"empty");
    for d in empty_dirs {
        h.update(d.as_bytes());
        h.update(&[0]);
    }
    ContentHash(*h.finalize().as_bytes())
}

impl Trigger {
    /// One-line label for `unwreck log`.
    pub fn label(&self) -> String {
        match self {
            Trigger::TurnStart { agent, prompt, .. } => format!(
                "{agent} turn start{}",
                prompt
                    .as_deref()
                    .map(|p| format!(": {:?}", truncate(p, 48)))
                    .unwrap_or_default()
            ),
            Trigger::PreTool {
                agent,
                tool,
                detail,
                ..
            } => format!(
                "{agent} before {tool}{}",
                detail
                    .as_deref()
                    .map(|d| format!(" `{}`", truncate(d, 40)))
                    .unwrap_or_default()
            ),
            Trigger::TurnEnd { agent, .. } => format!("{agent} turn end"),
            Trigger::Quiescent { agent } => format!("{} idle", agent.as_deref().unwrap_or("watch")),
            Trigger::Manual { label } => format!(
                "manual{}",
                label
                    .as_deref()
                    .map(|l| format!(": {l}"))
                    .unwrap_or_default()
            ),
            Trigger::PreRestore { restoring } => format!("before restore of #{restoring}"),
            Trigger::PostRestore { restored } => format!("after restore of #{restored}"),
        }
    }

    /// Snapshots that `unwreck undo` treats as turn boundaries.
    pub fn is_turn_boundary(&self) -> bool {
        matches!(
            self,
            Trigger::TurnStart { .. } | Trigger::Quiescent { .. } | Trigger::Manual { .. }
        )
    }

    /// Hook/watcher triggers may reuse the previous snapshot when the tree is unchanged.
    /// Manual pins and restore bookkeeping always write a new manifest.
    pub fn allows_noop_reuse(&self) -> bool {
        matches!(
            self,
            Trigger::TurnStart { .. }
                | Trigger::PreTool { .. }
                | Trigger::TurnEnd { .. }
                | Trigger::Quiescent { .. }
        )
    }

    /// Unchanged scans reuse only when this trigger is the same *kind* of event as `other`
    /// (and, for [`Trigger::PreTool`], the same tool name). A `PreToolUse` after a turn-start
    /// still writes a log line even if the tree has not changed yet.
    pub fn noop_compatible(&self, other: &Trigger) -> bool {
        match (self, other) {
            (Trigger::PreTool { tool: a, .. }, Trigger::PreTool { tool: b, .. }) => a == b,
            (Trigger::TurnStart { .. }, Trigger::TurnStart { .. }) => true,
            (Trigger::TurnEnd { .. }, Trigger::TurnEnd { .. }) => true,
            (Trigger::Quiescent { .. }, Trigger::Quiescent { .. }) => true,
            _ => false,
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.lines().next().unwrap_or("");
    if one_line.chars().count() <= n {
        one_line.to_owned()
    } else {
        one_line.chars().take(n).collect::<String>() + "…"
    }
}

/// Normalize an OS path into a `/`-joined [`RelPath`]. Returns `None` for
/// non-UTF-8 components or `..` / prefix components.
pub fn to_rel_path(p: &std::path::Path) -> Option<RelPath> {
    let mut parts = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_str()?.to_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_manifest_without_version_still_parses() {
        // Exactly what the pre-v2 skeleton wrote.
        let j = r#"{"id":3,"created_unix_ms":1,"trigger":{"trigger":"manual","label":null},"entries":[]}"#;
        let m: Manifest = serde_json::from_str(j).unwrap();
        assert_eq!((m.version, m.empty_dirs.len()), (0, 0));
        assert!(!m.tracks_dirs());
    }

    #[test]
    fn dirs_include_ancestors_and_empty_dirs() {
        let h = ContentHash([1; 32]);
        let m = Manifest {
            version: MANIFEST_VERSION,
            id: 1,
            created_unix_ms: 0,
            trigger: Trigger::Manual { label: None },
            entries: vec![Entry {
                path: "a/b/c.txt".into(),
                kind: EntryKind::File {
                    hash: h,
                    size: 1,
                    executable: false,
                },
            }],
            empty_dirs: vec!["x/y".into()],
        };
        let d: Vec<_> = m.dirs().into_iter().collect();
        assert_eq!(d, vec!["a", "a/b", "x", "x/y"]);
    }

    #[test]
    fn fingerprint_ignores_id_and_trigger() {
        let h = ContentHash([7u8; 32]);
        let e = vec![Entry {
            path: "a".into(),
            kind: EntryKind::File {
                hash: h,
                size: 1,
                executable: false,
            },
        }];
        let a = Manifest {
            version: 2,
            id: 1,
            created_unix_ms: 0,
            trigger: Trigger::Manual { label: None },
            entries: e.clone(),
            empty_dirs: vec!["d".into()],
        };
        let b = Manifest {
            version: 2,
            id: 9,
            created_unix_ms: 1,
            trigger: Trigger::TurnEnd {
                agent: "x".into(),
                session_id: None,
                turn_id: None,
            },
            entries: e,
            empty_dirs: vec!["d".into()],
        };
        assert_eq!(a.tree_fingerprint(), b.tree_fingerprint());
        let mut c = b.clone();
        c.empty_dirs.clear();
        assert_ne!(a.tree_fingerprint(), c.tree_fingerprint());
    }

    #[test]
    fn hash_roundtrip() {
        let h = ContentHash([7u8; 32]);
        let s = serde_json::to_string(&h).unwrap();
        let back: ContentHash = serde_json::from_str(&s).unwrap();
        assert_eq!(h, back);
    }
}
