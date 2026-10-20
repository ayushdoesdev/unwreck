//! Apply a restore plan to disk (atomic file replace, never touches `.git`).
use crate::io::{atomic_write_file, cleanup_stale_temps, remove_path, restore_symlink};
use crate::{Action, Plan};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use unwreck_core::EntryKind;
use unwreck_store::{Store, StoreError};

/// Failures applying a plan to disk.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// Blob missing or corrupt in the store.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Filesystem error on `path`.
    #[error("io on {path}: {source}")]
    Io {
        /// Path that failed.
        path: String,
        /// Underlying IO error.
        source: std::io::Error,
    },
}

/// What `apply` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Files/symlinks written or deleted, plus directories created.
    pub changed: usize,
    /// Directories removed because the restore left them empty (deepest first).
    pub removed_dirs: Vec<String>,
}

#[cfg(test)]
thread_local! {
    static CRASH_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static CRASH_DONE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test hook: next [`apply`] fails after `n` successful file writes/deletes.
#[cfg(test)]
pub fn inject_crash_after(n: usize) {
    CRASH_AFTER.set(Some(n));
    CRASH_DONE.set(0);
}

#[cfg(test)]
fn crash_tick() -> std::io::Result<()> {
    let n = CRASH_DONE.get() + 1;
    CRASH_DONE.set(n);
    if CRASH_AFTER.get() == Some(n) {
        CRASH_AFTER.set(None);
        return Err(std::io::Error::other("injected restore crash"));
    }
    Ok(())
}

fn git_path(p: &str) -> bool {
    p.split('/').any(|c| c == ".git")
}

/// Apply a plan. With `force`, conflicts are overwritten too (the PreRestore
/// snapshot still holds the user's version, so `unwreck redo` can bring it back).
pub fn apply(root: &Path, store: &Store, plan: &Plan, force: bool) -> Result<Applied, ApplyError> {
    let touched: Vec<&str> = plan
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::Write(e) => Some(e.path.as_str()),
            Action::Delete(p) => Some(p.as_str()),
            Action::Conflict { path, .. } if force => Some(path.as_str()),
            _ => None,
        })
        .filter(|p| !git_path(p))
        .collect();
    cleanup_stale_temps(root, touched.iter().copied());

    let mut changed = 0;
    let mut deleted: Vec<&str> = Vec::new();
    for a in &plan.actions {
        if let Action::RmDir(d) = a {
            if git_path(d) {
                continue;
            }
            if fs::remove_dir(root.join(d)).is_ok() {
                changed += 1;
                deleted.push(d.as_str());
            }
            continue;
        }
        if let Action::MkDir(d) = a {
            if git_path(d) {
                continue;
            }
            let abs = root.join(d);
            if !abs.is_dir() {
                fs::create_dir_all(&abs).map_err(|source| ApplyError::Io {
                    path: d.clone(),
                    source,
                })?;
                changed += 1;
            }
            continue;
        }
        let (path, want) = match a {
            Action::Write(e) => (e.path.as_str(), Some(e)),
            Action::Delete(p) => (p.as_str(), None),
            Action::Conflict { path, want } if force => (path.as_str(), want.as_ref()),
            _ => continue,
        };
        if git_path(path) {
            continue;
        }
        let abs = root.join(path);
        let io = |source| ApplyError::Io {
            path: path.to_owned(),
            source,
        };
        match want {
            None => {
                remove_path(&abs).map_err(io)?;
                deleted.push(path);
            }
            Some(e) => match &e.kind {
                EntryKind::File {
                    hash, executable, ..
                } => {
                    let bytes = store.get(hash)?;
                    atomic_write_file(&abs, &bytes, *executable).map_err(io)?;
                }
                EntryKind::Symlink { target } => {
                    restore_symlink(&abs, target).map_err(io)?;
                }
                EntryKind::Oversize { .. } => continue,
            },
        }
        changed += 1;
        #[cfg(test)]
        crash_tick().map_err(io)?;
    }
    let removed_dirs = match &plan.keep_dirs {
        Some(keep) => prune_empty_dirs(root, &deleted, keep),
        None => Vec::new(),
    };
    Ok(Applied {
        changed,
        removed_dirs,
    })
}

fn prune_empty_dirs(root: &Path, deleted: &[&str], keep: &BTreeSet<String>) -> Vec<String> {
    let mut cands: BTreeSet<&str> = BTreeSet::new();
    for p in deleted {
        let mut p = *p;
        while let Some(i) = p.rfind('/') {
            p = &p[..i];
            cands.insert(p);
        }
    }
    let mut cands: Vec<&str> = cands
        .into_iter()
        .filter(|d| !keep.contains(*d) && !git_path(d))
        .collect();
    cands.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    let mut removed = Vec::new();
    for d in cands {
        let abs = root.join(d);
        let is_real_dir = fs::symlink_metadata(&abs).is_ok_and(|m| m.is_dir());
        if is_real_dir
            && fs::read_dir(&abs).is_ok_and(|mut r| r.next().is_none())
            && fs::remove_dir(&abs).is_ok()
        {
            removed.push(d.to_owned());
        }
    }
    removed
}
