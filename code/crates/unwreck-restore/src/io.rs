//! Atomic replace, Windows retries, leftover temp cleanup.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Marker so leftover temps are easy to find: `.{name}.unwreck.tmp`.
pub(crate) const TEMP_SUFFIX: &str = ".unwreck.tmp";

/// `{dir}/.{filename}.unwreck.tmp`.
pub(crate) fn sibling_temp(dest: &Path) -> PathBuf {
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    dest.with_file_name(format!(".{name}{TEMP_SUFFIX}"))
}

pub(crate) fn is_restore_temp(name: &str) -> bool {
    name.ends_with(TEMP_SUFFIX) && name.starts_with('.')
}

fn retryable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) || cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)) // ACCESS_DENIED, SHARING_VIOLATION, LOCK_VIOLATION
}

pub(crate) fn retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let delays = [50u64, 100, 200];
    let mut last = None;
    for (i, d) in delays.iter().enumerate() {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if retryable(&e) && i + 1 < delays.len() => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(*d));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("retry exhausted")))
}

pub(crate) fn make_writable(p: &Path) {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = meta.permissions();
        let mode = perm.mode();
        if mode & 0o200 == 0 {
            perm.set_mode(mode | 0o200);
            let _ = fs::set_permissions(p, perm);
        }
    }
    #[cfg(not(unix))]
    {
        let mut perm = meta.permissions();
        if perm.readonly() {
            perm.set_readonly(false);
            let _ = fs::set_permissions(p, perm);
        }
    }
}

pub(crate) fn remove_path(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => {
            make_writable(p);
            retry(|| match fs::remove_file(p) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            })
        }
    }
}

/// Rename `from` over `to`. On Windows, remove `to` first (cannot replace an open/existing file).
pub(crate) fn replace_with(from: &Path, to: &Path) -> io::Result<()> {
    make_writable(to);
    #[cfg(windows)]
    {
        if fs::symlink_metadata(to).is_ok() {
            remove_path(to)?;
        }
        retry(|| fs::rename(from, to))
    }
    #[cfg(not(windows))]
    {
        retry(|| fs::rename(from, to))
    }
}

#[cfg(unix)]
fn fsync_dir(p: &Path) -> io::Result<()> {
    File::open(p)?.sync_all()
}
#[cfg(not(unix))]
fn fsync_dir(_p: &Path) -> io::Result<()> {
    Ok(())
}

/// Write `bytes` to a sibling temp, fsync, set mode, rename over `dest`.
pub(crate) fn atomic_write_file(dest: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = sibling_temp(dest);
    let _ = remove_path(&tmp);
    struct Guard<'a>(&'a Path);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0);
        }
    }
    let guard = Guard(&tmp);

    let existing = fs::symlink_metadata(dest).ok();
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    set_restored_mode(&tmp, existing.as_ref(), executable)?;
    replace_with(&tmp, dest)?;
    std::mem::forget(guard);
    if let Some(parent) = dest.parent() {
        let _ = fsync_dir(parent);
    }
    Ok(())
}

fn set_restored_mode(
    p: &Path,
    previous: Option<&fs::Metadata>,
    executable: bool,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let base = previous
            .filter(|m| !m.file_type().is_symlink())
            .map(|m| m.mode() & 0o777)
            .unwrap_or(if executable { 0o755 } else { 0o644 });
        let mode = if executable {
            base | 0o111
        } else {
            base & !0o111
        };
        fs::set_permissions(p, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (p, previous, executable);
    }
    Ok(())
}

pub(crate) fn restore_symlink(dest: &Path, target: &str) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = sibling_temp(dest);
    let _ = remove_path(&tmp);
    struct Guard<'a>(&'a Path);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0);
        }
    }
    let guard = Guard(&tmp);
    make_symlink(target, &tmp)?;
    replace_with(&tmp, dest)?;
    std::mem::forget(guard);
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Remove leftover `*.unwreck.tmp` next to paths this restore will touch.
pub(crate) fn cleanup_stale_temps(root: &Path, rels: impl Iterator<Item = impl AsRef<str>>) {
    let mut dirs = std::collections::BTreeSet::new();
    for rel in rels {
        let abs = root.join(rel.as_ref());
        if let Some(d) = abs.parent() {
            dirs.insert(d.to_path_buf());
        }
        let _ = remove_path(&sibling_temp(&abs));
    }
    for d in dirs {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_str().is_some_and(is_restore_temp) {
                let _ = remove_path(&e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_temp_is_next_to_dest() {
        let p = Path::new("/tmp/proj/src/lib.rs");
        assert_eq!(
            sibling_temp(p),
            Path::new("/tmp/proj/src/.lib.rs.unwreck.tmp")
        );
        assert!(is_restore_temp(".lib.rs.unwreck.tmp"));
        assert!(!is_restore_temp("lib.rs"));
    }
}
