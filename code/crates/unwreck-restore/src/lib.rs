//! Restore planning with conflict detection.
//!
//! Inputs (PLAN.md "Restore and redo"):
//! * `target`  – the snapshot the user wants back.
//! * `last`    – the most recent snapshot unwreck recorded *before* the restore.
//! * `current` – a fresh `PreRestore` snapshot of the disk right now (also the redo point).
//!
//! A path is safe to rewrite only if `current == last` for it, i.e. nobody touched it
//! after unwreck last saw it. Otherwise it is a conflict and is skipped unless forced.
//!
//! Directories (manifest v2+):
//! * Empty directories recorded in `target` are recreated (`MkDir`).
//! * Empty directories that are not in `target` are removed (`RmDir`) only if `last` also
//!   recorded them as empty, i.e. the same T/L/C rule as files: an empty directory you
//!   created after the last snapshot is left alone.
//! * After applying, directories that the restore emptied (ancestors of deleted paths)
//!   are removed with `remove_dir` (which refuses non-empty dirs), unless the directory
//!   exists in `target`, or is `.git` / inside `.git`. A directory you created later that
//!   holds files is never empty, so it is never removed.
//! * If `target` is an older manifest without directory tracking, nothing is pruned:
//!   we cannot tell whether an empty directory existed in it.
#![warn(missing_docs)]

mod apply;
mod io;
#[cfg(test)]
mod proptest_tree;

#[cfg(test)]
pub use apply::inject_crash_after;
pub use apply::{Applied, ApplyError, apply};

use std::collections::BTreeSet;
use unwreck_core::{Entry, EntryKind, Manifest};

/// One planned change to the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write (create or overwrite) the file/symlink from the target snapshot.
    Write(Entry),
    /// Remove a path that did not exist in the target snapshot.
    Delete(String),
    /// Path changed after the last recorded snapshot; left alone unless forced.
    Conflict {
        /// Path in the worktree.
        path: String,
        /// Contents from the target, if the target had this path.
        want: Option<Entry>,
    },
    /// Target had the file only as metadata (over size cap); cannot restore contents.
    Unrecoverable(String),
    /// Recreate an empty directory recorded in the target snapshot.
    MkDir(String),
    /// Remove an empty directory that is not in the target and that unwreck already saw
    /// (empty) in the last snapshot.
    RmDir(String),
}

/// Ordered restore actions plus directory-prune context.
#[derive(Debug, Default)]
pub struct Plan {
    /// Per-path actions (files, then mkdir/rmdir).
    pub actions: Vec<Action>,
    /// Directories that exist in the target; `None` = target does not track dirs, so
    /// pruning is disabled.
    pub keep_dirs: Option<BTreeSet<String>>,
}

impl Plan {
    /// Number of [`Action::Conflict`] entries.
    pub fn conflicts(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, Action::Conflict { .. }))
            .count()
    }

    /// Directories that apply would try to `rmdir` after deleting files (deepest first).
    /// Dry-run uses this so undo/restore show folder removals before touching disk.
    pub fn preview_pruned_dirs(&self) -> Vec<String> {
        let Some(keep) = &self.keep_dirs else {
            return Vec::new();
        };
        let mut deleted: Vec<&str> = Vec::new();
        for a in &self.actions {
            match a {
                Action::Delete(p) | Action::RmDir(p) => deleted.push(p.as_str()),
                _ => {}
            }
        }
        let mut cands: BTreeSet<&str> = BTreeSet::new();
        for p in &deleted {
            let mut p = *p;
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                cands.insert(p);
            }
        }
        let mut cands: Vec<String> = cands
            .into_iter()
            .filter(|d| !keep.contains(*d) && !is_git_path(d))
            .map(str::to_owned)
            .collect();
        cands.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
        cands
    }
}

fn kind_of(m: &Manifest, p: &str) -> Option<EntryKind> {
    m.get(p).map(|e| e.kind.clone())
}

/// Build a restore plan. `only`, if set, restricts file actions to those paths.
pub fn plan(
    target: &Manifest,
    last: &Manifest,
    current: &Manifest,
    only: Option<&BTreeSet<String>>,
) -> Plan {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    paths.extend(target.entries.iter().map(|e| e.path.as_str()));
    paths.extend(current.entries.iter().map(|e| e.path.as_str()));
    let mut actions = Vec::new();
    for p in paths {
        if only.is_some_and(|o| !o.contains(p)) {
            continue;
        }
        let (t, l, c) = (kind_of(target, p), kind_of(last, p), kind_of(current, p));
        if t == c {
            continue;
        }
        if matches!(t, Some(EntryKind::Oversize { .. })) {
            actions.push(Action::Unrecoverable(p.to_owned()));
            continue;
        }
        let want = target.get(p).cloned();
        if c != l {
            actions.push(Action::Conflict {
                path: p.to_owned(),
                want,
            });
            continue;
        }
        actions.push(match want {
            Some(e) => Action::Write(e),
            None => Action::Delete(p.to_owned()),
        });
    }
    if target.tracks_dirs() {
        let have = current.dirs();
        for d in &target.empty_dirs {
            if !have.contains(d) && only.is_none_or(|o| o.contains(d)) && !is_git_path(d) {
                actions.push(Action::MkDir(d.clone()));
            }
        }
        let keep = target.dirs();
        for d in &current.empty_dirs {
            let seen = last.tracks_dirs() && last.empty_dirs.binary_search(d).is_ok();
            if seen && !keep.contains(d) && only.is_none_or(|o| o.contains(d)) && !is_git_path(d) {
                actions.push(Action::RmDir(d.clone()));
            }
        }
    }
    Plan {
        actions,
        keep_dirs: target.tracks_dirs().then(|| target.dirs()),
    }
}

fn is_git_path(p: &str) -> bool {
    p.split('/').any(|c| c == ".git")
}

#[cfg(test)]
mod tests {
    use super::*;
    use unwreck_core::{ContentHash, Trigger};

    fn m(id: u64, files: &[(&str, u8)]) -> Manifest {
        Manifest {
            version: unwreck_core::MANIFEST_VERSION,
            empty_dirs: vec![],
            id,
            created_unix_ms: 0,
            trigger: Trigger::Manual { label: None },
            entries: files
                .iter()
                .map(|(p, h)| Entry {
                    path: (*p).into(),
                    kind: EntryKind::File {
                        hash: ContentHash([*h; 32]),
                        size: 1,
                        executable: false,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn deleted_file_is_restored_and_user_edit_is_a_conflict() {
        let target = m(1, &[("a", 1), ("b", 1)]);
        let last = m(2, &[("b", 2)]); // agent deleted a, changed b
        let current = m(3, &[("b", 3)]); // user then edited b again
        let p = plan(&target, &last, &current, None);
        assert!(matches!(&p.actions[0], Action::Write(e) if e.path == "a"));
        assert!(matches!(&p.actions[1], Action::Conflict { path, .. } if path == "b"));
        assert_eq!(p.conflicts(), 1);
    }

    #[test]
    fn file_created_after_target_is_deleted() {
        let target = m(1, &[]);
        let last = m(2, &[("new", 1)]);
        let current = m(3, &[("new", 1)]);
        let p = plan(&target, &last, &current, None);
        assert_eq!(p.actions, vec![Action::Delete("new".into())]);
    }

    /// End-to-end through the real store and scanner.
    mod fs_tests {
        use super::super::*;
        use std::fs;
        use std::path::Path;
        use unwreck_core::Trigger;
        use unwreck_scan::{ScanPolicy, snapshot};
        use unwreck_store::Store;

        struct Fx {
            wt: tempfile::TempDir,
            _st: tempfile::TempDir,
            store: Store,
        }
        impl Fx {
            fn new() -> Self {
                let wt = tempfile::tempdir().unwrap();
                let st = tempfile::tempdir().unwrap();
                let store = Store::open(st.path()).unwrap();
                Fx { wt, _st: st, store }
            }
            fn r(&self) -> &Path {
                self.wt.path()
            }
            fn snap(&self) -> Manifest {
                let _l = self.store.lock().unwrap();
                snapshot(
                    self.r(),
                    &self.store,
                    &ScanPolicy::default(),
                    Trigger::Manual { label: None },
                )
                .unwrap()
                .manifest
            }
            /// Same sequence as the CLI: last = latest, current = fresh scan, plan, apply.
            fn restore_to(&self, target: &Manifest) -> Applied {
                let last = self.store.latest().unwrap().unwrap();
                let current = self.snap();
                let p = plan(target, &last, &current, None);
                let _l = self.store.lock().unwrap();
                apply(self.r(), &self.store, &p, false).unwrap()
            }
            fn write(&self, p: &str, s: &str) {
                let abs = self.r().join(p);
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                fs::write(abs, s).unwrap();
            }
        }

        #[test]
        fn undo_of_created_tree_removes_emptied_dirs() {
            let fx = Fx::new();
            fx.write("src/lib.rs", "a");
            let t = fx.snap();
            fx.write("src/gen/deep/x.rs", "gen");
            fx.write("src/gen/y.rs", "gen");
            fx.snap();
            let a = fx.restore_to(&t);
            assert_eq!(
                a.removed_dirs,
                vec!["src/gen/deep".to_string(), "src/gen".to_string()]
            );
            assert!(!fx.r().join("src/gen").exists());
            assert!(fx.r().join("src/lib.rs").exists()); // src is in target: kept
        }

        #[test]
        fn dirs_in_target_and_user_dirs_with_files_are_kept() {
            let fx = Fx::new();
            fx.write("keep/a", "a");
            let t = fx.snap();
            fx.write("keep/b", "b"); // agent adds a file to a dir that exists in target
            fx.write("gen/x", "x"); // agent creates gen/x
            fx.snap();
            fx.write("gen/mine.txt", "user"); // user adds a file after the last snapshot
            let a = fx.restore_to(&t);
            assert!(fx.r().join("keep").is_dir() && !fx.r().join("keep/b").exists());
            assert!(
                fx.r().join("gen/mine.txt").exists(),
                "user file must survive"
            );
            assert!(a.removed_dirs.is_empty(), "{:?}", a.removed_dirs);
        }

        #[test]
        fn empty_dirs_in_target_are_recreated_and_not_pruned() {
            let fx = Fx::new();
            fs::create_dir_all(fx.r().join("logs")).unwrap();
            fx.write("cache/a", "a");
            fs::remove_file(fx.r().join("cache/a")).unwrap(); // cache/ is now empty
            let t = fx.snap();
            assert_eq!(t.empty_dirs, vec!["cache".to_string(), "logs".to_string()]);
            fs::remove_dir(fx.r().join("logs")).unwrap();
            fx.write("cache/tmp", "t");
            fx.snap();
            let a = fx.restore_to(&t);
            assert!(fx.r().join("logs").is_dir(), "recreated");
            assert!(
                fx.r().join("cache").is_dir() && !fx.r().join("cache/tmp").exists(),
                "emptied but kept"
            );
            assert!(a.removed_dirs.is_empty());
        }

        #[test]
        fn empty_dir_seen_by_last_is_removed_but_user_created_one_is_kept() {
            let fx = Fx::new();
            fx.write("a", "a");
            let t = fx.snap();
            fs::create_dir_all(fx.r().join("agent/empty")).unwrap(); // agent makes it
            fx.snap(); // unwreck sees it
            fs::create_dir_all(fx.r().join("mine")).unwrap(); // user makes one later
            let a = fx.restore_to(&t);
            assert!(
                !fx.r().join("agent").exists(),
                "agent/empty and then agent/ removed"
            );
            assert!(fx.r().join("mine").is_dir());
            assert_eq!(a.removed_dirs, vec!["agent".to_string()]);
        }

        #[test]
        fn git_dir_is_never_touched() {
            let fx = Fx::new();
            fx.write("a", "a");
            let t = fx.snap();
            fs::create_dir_all(fx.r().join(".git/objects/empty")).unwrap();
            fx.write("sub/.git/HEAD", "x"); // nested repo-ish dir: .git is never scanned
            fx.snap();
            fx.restore_to(&t);
            assert!(fx.r().join(".git/objects/empty").is_dir());
            assert!(fx.r().join("sub/.git/HEAD").exists());
        }

        #[test]
        fn legacy_target_without_dir_tracking_prunes_nothing() {
            let fx = Fx::new();
            fx.write("a", "a");
            let mut t = fx.snap();
            t.version = 0; // as written by the pre-v2 skeleton
            t.empty_dirs.clear();
            fx.write("gen/x", "x");
            fx.snap();
            let a = fx.restore_to(&t);
            assert!(!fx.r().join("gen/x").exists());
            assert!(fx.r().join("gen").is_dir());
            assert!(a.removed_dirs.is_empty());
        }

        #[test]
        fn crash_mid_restore_never_truncates_a_file() {
            let fx = Fx::new();
            for name in ["a", "b", "c", "d"] {
                fx.write(name, "old-content-xxxx");
            }
            let t = fx.snap();
            for name in ["a", "b", "c", "d"] {
                fx.write(name, "new-content-yyyy");
            }
            fx.snap();
            crate::inject_crash_after(2);
            let last = fx.store.latest().unwrap().unwrap();
            let current = fx.snap();
            let p = plan(&t, &last, &current, None);
            let _l = fx.store.lock().unwrap();
            assert!(apply(fx.r(), &fx.store, &p, false).is_err());
            let mut old = 0;
            let mut new = 0;
            for name in ["a", "b", "c", "d"] {
                let s = fs::read_to_string(fx.r().join(name)).unwrap();
                assert!(
                    s == "old-content-xxxx" || s == "new-content-yyyy",
                    "{name} was truncated: {s:?}"
                );
                if s == "old-content-xxxx" {
                    old += 1;
                } else {
                    new += 1;
                }
                assert!(
                    !fx.r()
                        .join(name)
                        .with_file_name(format!(".{name}.unwreck.tmp"))
                        .exists(),
                    "temp leftover for {name}"
                );
            }
            assert_eq!((new, old), (2, 2));
            // Next restore finishes and sweeps temps.
            apply(fx.r(), &fx.store, &p, false).unwrap();
            for name in ["a", "b", "c", "d"] {
                assert_eq!(
                    fs::read_to_string(fx.r().join(name)).unwrap(),
                    "old-content-xxxx"
                );
            }
        }

        #[test]
        fn leftover_temp_is_removed_on_next_apply() {
            let fx = Fx::new();
            fx.write("a", "keep");
            let t = fx.snap();
            let tmp = fx.r().join(".a.unwreck.tmp");
            fs::write(&tmp, "partial").unwrap();
            fx.write("b", "x");
            fx.snap();
            fx.restore_to(&t);
            assert!(!tmp.exists());
            assert!(!fx.r().join("b").exists());
        }

        #[cfg(unix)]
        #[test]
        fn executable_bit_survives_restore() {
            use std::os::unix::fs::PermissionsExt;
            let fx = Fx::new();
            fx.write("bin.sh", "#!/bin/sh\n");
            fs::set_permissions(fx.r().join("bin.sh"), fs::Permissions::from_mode(0o755)).unwrap();
            fx.write("readme", "text");
            fs::set_permissions(fx.r().join("readme"), fs::Permissions::from_mode(0o644)).unwrap();
            let t = fx.snap();
            fs::set_permissions(fx.r().join("bin.sh"), fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(fx.r().join("readme"), fs::Permissions::from_mode(0o755)).unwrap();
            fx.snap();
            fx.restore_to(&t);
            let bin = fs::metadata(fx.r().join("bin.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111;
            let rd = fs::metadata(fx.r().join("readme"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111;
            assert_ne!(bin, 0, "script should be executable");
            assert_eq!(rd, 0, "readme should not be executable");
        }

        #[cfg(unix)]
        #[test]
        fn symlink_round_trips() {
            let fx = Fx::new();
            fx.write("target.txt", "hi");
            std::os::unix::fs::symlink("target.txt", fx.r().join("link")).unwrap();
            let t = fx.snap();
            fs::remove_file(fx.r().join("link")).unwrap();
            fx.write("link", "not a link");
            fx.snap();
            fx.restore_to(&t);
            let meta = fs::symlink_metadata(fx.r().join("link")).unwrap();
            assert!(meta.file_type().is_symlink());
            assert_eq!(
                fs::read_link(fx.r().join("link")).unwrap(),
                Path::new("target.txt")
            );
            assert_eq!(fs::read_to_string(fx.r().join("link")).unwrap(), "hi");
        }

        #[cfg(unix)]
        #[test]
        fn read_only_file_can_be_replaced() {
            use std::os::unix::fs::PermissionsExt;
            let fx = Fx::new();
            fx.write("a", "old");
            let t = fx.snap();
            fx.write("a", "new");
            fs::set_permissions(fx.r().join("a"), fs::Permissions::from_mode(0o444)).unwrap();
            fx.snap();
            fx.restore_to(&t);
            assert_eq!(fs::read_to_string(fx.r().join("a")).unwrap(), "old");
        }
    }
}
