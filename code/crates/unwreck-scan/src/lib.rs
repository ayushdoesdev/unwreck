//! Walk a worktree and capture it into the store as a snapshot.
//!
//! MVP policy (PLAN.md "What gets captured"):
//! * `.git/` and the store itself are never captured.
//! * `.gitignore` is respected by default, BUT small files matching `always_include`
//!   (default `.env`, `.env.*`) are captured even when ignored: losing an untracked
//!   `.env` is exactly the failure users report.
//! * Heavy directories (`node_modules`, `target`, ...) are never descended.
//! * Files over `max_file_bytes` are recorded as `Oversize` (metadata only).
//! * Directories with no children at all are recorded in `Manifest::empty_dirs`.
//!
//! Stat cache: `statcache.bin` in the store maps path -> (size, mtime, ctime, inode) -> hash.
//! A file whose stat matches is not read or rehashed. Entries are only cached when the
//! file's mtime is at least `RACY_SECS` older than the scan start, so a write landing in
//! the same timestamp tick as the scan can never be missed (the "racy git" problem).
//! The cache is checksummed; a missing or damaged cache just means a full rehash.
#![warn(missing_docs)]

use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use unwreck_core::{
    ContentHash, Entry, EntryKind, Manifest, Trigger, to_rel_path, tree_fingerprint,
};
use unwreck_store::{Store, StoreError};

/// Failures walking the worktree or writing the store.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Underlying store error.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// `ignore` crate walk error.
    #[error("walk: {0}")]
    Walk(#[from] ignore::Error),
    /// Filesystem error while reading a file or directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// What to capture. See the crate docs for the default policy.
#[derive(Clone, Debug)]
pub struct ScanPolicy {
    /// Honor `.gitignore`, `.ignore`, and global excludes.
    pub respect_gitignore: bool,
    /// Globs captured even when ignored (default: `.env` / `.env.*`).
    pub always_include: Vec<String>,
    /// Directory names never descended into.
    pub heavy_dirs: Vec<String>,
    /// Files larger than this are stored as `Oversize` only.
    pub max_file_bytes: u64,
}

impl Default for ScanPolicy {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            always_include: vec![
                ".env".into(),
                ".env.*".into(),
                "**/.env".into(),
                "**/.env.*".into(),
            ],
            heavy_dirs: [
                "node_modules",
                "target",
                ".venv",
                "venv",
                "__pycache__",
                "dist",
                "build",
                ".next",
                ".turbo",
                ".gradle",
            ]
            .map(String::from)
            .to_vec(),
            max_file_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Outcome of one [`snapshot`].
pub struct ScanReport {
    /// Written or reused snapshot.
    pub manifest: Manifest,
    /// Number of file/symlink/oversize entries.
    pub files: usize,
    /// Blobs that were not already in the store.
    pub new_blobs: usize,
    /// Files actually read and hashed (stat-cache misses).
    pub hashed: usize,
    /// True when the tree matched the previous snapshot and no new manifest was written.
    pub reused: bool,
}

const STATCACHE: &str = "statcache.bin";
const STATCACHE_MAGIC: &[u8; 8] = b"UWSC\x01\0\0\0";
const RACY_SECS: i64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    mtime_s: i64,
    mtime_ns: u32,
    ctime_s: i64,
    ctime_ns: u32,
    ino: u64,
}

#[cfg(unix)]
fn stamp(m: &fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    Stamp {
        size: m.len(),
        mtime_s: m.mtime(),
        mtime_ns: m.mtime_nsec() as u32,
        ctime_s: m.ctime(),
        ctime_ns: m.ctime_nsec() as u32,
        ino: m.ino(),
    }
}
#[cfg(not(unix))]
fn stamp(m: &fs::Metadata) -> Stamp {
    let d = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    Stamp {
        size: m.len(),
        mtime_s: d.as_secs() as i64,
        mtime_ns: d.subsec_nanos(),
        ctime_s: 0,
        ctime_ns: 0,
        ino: 0,
    }
}

type StatCache = HashMap<String, (Stamp, ContentHash)>;

fn encode_cache(c: &StatCache) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + c.len() * 96);
    out.extend_from_slice(STATCACHE_MAGIC);
    out.extend_from_slice(&(c.len() as u64).to_le_bytes());
    for (p, (st, h)) in c {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(&st.size.to_le_bytes());
        out.extend_from_slice(&st.mtime_s.to_le_bytes());
        out.extend_from_slice(&st.mtime_ns.to_le_bytes());
        out.extend_from_slice(&st.ctime_s.to_le_bytes());
        out.extend_from_slice(&st.ctime_ns.to_le_bytes());
        out.extend_from_slice(&st.ino.to_le_bytes());
        out.extend_from_slice(&h.0);
    }
    let sum = blake3::hash(&out);
    out.extend_from_slice(sum.as_bytes());
    out
}

fn decode_cache(b: &[u8]) -> Option<StatCache> {
    if b.len() < 16 + 32 || &b[..8] != STATCACHE_MAGIC {
        return None;
    }
    let (body, sum) = b.split_at(b.len() - 32);
    if blake3::hash(body).as_bytes() != sum {
        return None;
    }
    let n = u64::from_le_bytes(body[8..16].try_into().ok()?) as usize;
    let mut c = HashMap::with_capacity(n);
    let mut r = &body[16..];
    fn take<'a>(r: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        if r.len() < n {
            return None;
        }
        let (a, b) = r.split_at(n);
        *r = b;
        Some(a)
    }
    for _ in 0..n {
        let plen = u32::from_le_bytes(take(&mut r, 4)?.try_into().ok()?) as usize;
        let p = std::str::from_utf8(take(&mut r, plen)?).ok()?.to_owned();
        let size = u64::from_le_bytes(take(&mut r, 8)?.try_into().ok()?);
        let mtime_s = i64::from_le_bytes(take(&mut r, 8)?.try_into().ok()?);
        let mtime_ns = u32::from_le_bytes(take(&mut r, 4)?.try_into().ok()?);
        let ctime_s = i64::from_le_bytes(take(&mut r, 8)?.try_into().ok()?);
        let ctime_ns = u32::from_le_bytes(take(&mut r, 4)?.try_into().ok()?);
        let ino = u64::from_le_bytes(take(&mut r, 8)?.try_into().ok()?);
        let h = ContentHash(take(&mut r, 32)?.try_into().ok()?);
        c.insert(
            p,
            (
                Stamp {
                    size,
                    mtime_s,
                    mtime_ns,
                    ctime_s,
                    ctime_ns,
                    ino,
                },
                h,
            ),
        );
    }
    r.is_empty().then_some(c)
}

fn is_heavy(name: &str, policy: &ScanPolicy) -> bool {
    name == ".git" || policy.heavy_dirs.iter().any(|h| h == name)
}

struct ToHash {
    rel: String,
    abs: PathBuf,
    st: Stamp,
    executable: bool,
}

struct WalkAcc {
    entries: HashMap<String, Entry>,
    empty_dirs: Vec<String>,
    to_hash: Vec<ToHash>,
    new_cache: StatCache,
}

/// Snapshot `root` into `store`. Caller should hold `store.lock()`.
pub fn snapshot(
    root: &Path,
    store: &Store,
    policy: &ScanPolicy,
    trigger: Trigger,
) -> Result<ScanReport, ScanError> {
    let start_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let timing = std::env::var_os("UNWRECK_TIMING").is_some();
    let t0 = std::time::Instant::now();
    let old_cache = store
        .read_aux(STATCACHE)
        .and_then(|b| decode_cache(&b))
        .unwrap_or_default();

    let packed = store.packed_hashes();
    let acc = walk_tree(root, store, policy, &old_cache, &packed, start_s)?;
    let WalkAcc {
        mut entries,
        mut empty_dirs,
        to_hash,
        mut new_cache,
    } = acc;

    let hashed = to_hash.len();
    let mut new_blobs = 0;
    if !to_hash.is_empty() {
        let hashed_files: Result<Vec<_>, ScanError> = to_hash
            .into_par_iter()
            .map(|t| {
                let bytes = fs::read(&t.abs)?;
                let h = Store::hash_bytes(&bytes);
                Ok((t, bytes, h))
            })
            .collect();
        for (t, bytes, h) in hashed_files? {
            if store.put_hashed(h, &bytes)? {
                new_blobs += 1;
            }
            let size = bytes.len() as u64;
            if t.st.mtime_s + RACY_SECS <= start_s && size == t.st.size {
                new_cache.insert(t.rel.clone(), (t.st, h));
            }
            entries.insert(
                t.rel.clone(),
                Entry {
                    path: t.rel,
                    kind: EntryKind::File {
                        hash: h,
                        size,
                        executable: t.executable,
                    },
                },
            );
        }
    }

    let mut entries: Vec<Entry> = entries.into_values().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    empty_dirs.sort();
    empty_dirs.dedup();
    let files = entries.len();
    let fp = tree_fingerprint(&entries, &empty_dirs);
    let t_walk = t0.elapsed();

    let (manifest, reused) = if trigger.allows_noop_reuse()
        && let Some(tip) = store.tip()?
        && tip.tree == fp
        && trigger.noop_compatible(&tip.trigger)
    {
        (store.reused_manifest(&tip, entries, empty_dirs), true)
    } else {
        (store.write_manifest(trigger, entries, empty_dirs)?, false)
    };
    if new_cache != old_cache {
        store.write_aux(STATCACHE, &encode_cache(&new_cache))?;
    }
    if timing {
        eprintln!(
            "unwreck: scan {files} files, hashed {hashed}, new blobs {new_blobs}, reused {reused}, cache {} -> {}; walk+hash {:?}, commit+manifest+cache {:?}",
            old_cache.len(),
            new_cache.len(),
            t_walk,
            t0.elapsed() - t_walk
        );
    }
    Ok(ScanReport {
        manifest,
        files,
        new_blobs,
        hashed,
        reused,
    })
}

struct WalkCtx<'a> {
    root: &'a Path,
    store: &'a Store,
    policy: &'a ScanPolicy,
    old_cache: &'a StatCache,
    packed: &'a HashSet<ContentHash>,
    start_s: i64,
    record_empty_dirs: bool,
    skip_if_present: bool,
    acc: &'a Mutex<WalkAcc>,
    err: &'a Mutex<Option<ScanError>>,
}

fn walk_tree(
    root: &Path,
    store: &Store,
    policy: &ScanPolicy,
    old_cache: &StatCache,
    packed: &HashSet<ContentHash>,
    start_s: i64,
) -> Result<WalkAcc, ScanError> {
    let acc = Mutex::new(WalkAcc {
        entries: HashMap::new(),
        empty_dirs: Vec::new(),
        to_hash: Vec::new(),
        new_cache: HashMap::with_capacity(old_cache.len()),
    });
    let err: Mutex<Option<ScanError>> = Mutex::new(None);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let mut ctx = WalkCtx {
        root,
        store,
        policy,
        old_cache,
        packed,
        start_s,
        record_empty_dirs: true,
        skip_if_present: false,
        acc: &acc,
        err: &err,
    };
    run_walk(&ctx, threads, policy.respect_gitignore, None)?;
    // Whitelist-only overrides invert the matcher: unmatched *files* are ignored, so this
    // pass yields `.env` (even when gitignored) without re-processing the rest of the tree.
    // Skip it when the tree has no ignore files: the first walk already saw everything.
    let has_ignore = root.join(".gitignore").is_file() || root.join(".ignore").is_file();
    if policy.respect_gitignore && !policy.always_include.is_empty() && has_ignore {
        let mut ob = OverrideBuilder::new(root);
        for g in &policy.always_include {
            ob.add(g)?;
        }
        ctx.record_empty_dirs = false;
        ctx.skip_if_present = true;
        run_walk(&ctx, threads, false, Some(ob.build()?))?;
    }

    if let Some(e) = err.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    Ok(acc.into_inner().unwrap_or_else(|e| e.into_inner()))
}

fn run_walk(
    ctx: &WalkCtx<'_>,
    threads: usize,
    gitignore: bool,
    overrides: Option<ignore::overrides::Override>,
) -> Result<(), ScanError> {
    if ctx.err.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return Ok(());
    }
    let mut b = WalkBuilder::new(ctx.root);
    b.hidden(false)
        .git_ignore(gitignore)
        .git_exclude(gitignore)
        .git_global(gitignore)
        .ignore(gitignore)
        .parents(gitignore)
        .require_git(false)
        .follow_links(false)
        .threads(threads);
    if let Some(o) = overrides {
        b.overrides(o);
    }
    let p = ctx.policy.clone();
    b.filter_entry(move |d| {
        let is_dir = d.file_type().is_some_and(|t| t.is_dir());
        !(is_dir && d.file_name().to_str().is_some_and(|n| is_heavy(n, &p)))
    });
    b.build_parallel().run(|| {
        Box::new(move |result| {
            if ctx.err.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                return WalkState::Quit;
            }
            match process_entry(ctx, result) {
                Ok(()) => WalkState::Continue,
                Err(e) => {
                    *ctx.err.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    WalkState::Quit
                }
            }
        })
    });
    Ok(())
}

fn process_entry(
    ctx: &WalkCtx<'_>,
    result: Result<ignore::DirEntry, ignore::Error>,
) -> Result<(), ScanError> {
    let dent = result?;
    let ft = match dent.file_type() {
        Some(ft) => ft,
        None => return Ok(()),
    };
    let rel = match dent
        .path()
        .strip_prefix(ctx.root)
        .ok()
        .and_then(to_rel_path)
    {
        Some(r) if !r.is_empty() => r,
        _ => return Ok(()),
    };
    if ft.is_dir() {
        if ctx.record_empty_dirs && fs::read_dir(dent.path())?.next().is_none() {
            ctx.acc
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .empty_dirs
                .push(rel);
        }
        return Ok(());
    }
    if ctx.skip_if_present {
        let g = ctx.acc.lock().unwrap_or_else(|e| e.into_inner());
        if g.entries.contains_key(&rel) || g.to_hash.iter().any(|t| t.rel == rel) {
            return Ok(());
        }
    }
    let kind = if ft.is_symlink() {
        let target = fs::read_link(dent.path())?;
        EntryKind::Symlink {
            target: target.to_string_lossy().into_owned(),
        }
    } else {
        let meta = dent.metadata()?;
        if meta.len() > ctx.policy.max_file_bytes {
            EntryKind::Oversize { size: meta.len() }
        } else {
            let st = stamp(&meta);
            let blob_ok = |h: &ContentHash| ctx.packed.contains(h) || ctx.store.has(h);
            let cached = ctx
                .old_cache
                .get(&rel)
                .filter(|(s, h)| *s == st && blob_ok(h))
                .map(|(_, h)| *h);
            match cached {
                Some(h) => {
                    if st.mtime_s + RACY_SECS <= ctx.start_s {
                        ctx.acc
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .new_cache
                            .insert(rel.clone(), (st, h));
                    }
                    EntryKind::File {
                        hash: h,
                        size: st.size,
                        executable: is_executable(&meta),
                    }
                }
                None => {
                    ctx.acc
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .to_hash
                        .push(ToHash {
                            rel,
                            abs: dent.path().to_path_buf(),
                            st,
                            executable: is_executable(&meta),
                        });
                    return Ok(());
                }
            }
        }
    };
    ctx.acc
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .insert(rel.clone(), Entry { path: rel, kind });
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_ignored_env_but_not_node_modules() {
        let wt = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let r = wt.path();
        fs::write(r.join(".gitignore"), ".env\nout.log\n").unwrap();
        fs::write(r.join(".env"), "SECRET=1").unwrap();
        fs::write(r.join("out.log"), "noise").unwrap();
        fs::write(r.join("main.rs"), "fn main(){}").unwrap();
        fs::create_dir_all(r.join("node_modules/x")).unwrap();
        fs::write(r.join("node_modules/x/i.js"), "x").unwrap();
        let store = Store::open(st.path()).unwrap();
        let _l = store.lock().unwrap();
        let rep = snapshot(
            r,
            &store,
            &ScanPolicy::default(),
            Trigger::Manual { label: None },
        )
        .unwrap();
        let paths: Vec<_> = rep
            .manifest
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, vec![".env", ".gitignore", "main.rs"]);
    }

    fn age(p: &Path, secs: u64) {
        let t = SystemTime::now() - std::time::Duration::from_secs(secs);
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn records_empty_dirs_only() {
        let wt = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let r = wt.path();
        fs::create_dir_all(r.join("empty/nested")).unwrap();
        fs::create_dir_all(r.join("full")).unwrap();
        fs::write(r.join("full/a"), "a").unwrap();
        fs::create_dir_all(r.join("node_modules/empty")).unwrap();
        let store = Store::open(st.path()).unwrap();
        let _l = store.lock().unwrap();
        let rep = snapshot(
            r,
            &store,
            &ScanPolicy::default(),
            Trigger::Manual { label: None },
        )
        .unwrap();
        assert_eq!(rep.manifest.empty_dirs, vec!["empty/nested".to_string()]);
        assert!(rep.manifest.tracks_dirs());
    }

    #[test]
    fn stat_cache_skips_unchanged_and_catches_changes() {
        let wt = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let r = wt.path();
        for i in 0..20 {
            fs::write(r.join(format!("f{i}")), format!("v1 {i}")).unwrap();
            age(&r.join(format!("f{i}")), 60);
        }
        fs::write(r.join("fresh"), "just written").unwrap(); // racy: must not be cached
        let store = Store::open(st.path()).unwrap();
        let _l = store.lock().unwrap();
        let p = ScanPolicy::default();
        let m = |t: &str| Trigger::Manual {
            label: Some(t.into()),
        };
        let r1 = snapshot(r, &store, &p, m("1")).unwrap();
        assert_eq!(r1.hashed, 21);
        let r2 = snapshot(r, &store, &p, m("2")).unwrap();
        assert_eq!(r2.hashed, 1, "only the racy file is rehashed");
        assert_eq!(r1.manifest.entries, r2.manifest.entries);
        // Same size, content changed, mtime moved: must be detected.
        fs::write(r.join("f3"), "v2 3").unwrap();
        age(&r.join("f3"), 30);
        let r3 = snapshot(r, &store, &p, m("3")).unwrap();
        assert_eq!(r3.hashed, 2);
        let h = |m: &Manifest| m.get("f3").unwrap().kind.clone();
        assert_ne!(h(&r2.manifest), h(&r3.manifest));
        // A damaged cache is ignored, never trusted.
        fs::write(st.path().join(STATCACHE), b"garbage").unwrap();
        let r4 = snapshot(r, &store, &p, m("4")).unwrap();
        assert_eq!(r4.hashed, 21);
        assert_eq!(r4.manifest.entries, r3.manifest.entries);
    }

    fn turn(label: &str) -> Trigger {
        Trigger::TurnStart {
            agent: "t".into(),
            session_id: None,
            turn_id: None,
            prompt: Some(label.into()),
        }
    }
    fn pretool() -> Trigger {
        Trigger::PreTool {
            agent: "t".into(),
            session_id: None,
            turn_id: None,
            tool: "Bash".into(),
            detail: Some("rm".into()),
        }
    }

    #[test]
    fn unchanged_hook_snapshot_reuses_id_and_keeps_old_manifests() {
        let wt = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let r = wt.path();
        fs::write(r.join("a.rs"), "fn a(){}").unwrap();
        age(&r.join("a.rs"), 60);
        let store = Store::open(st.path()).unwrap();
        let _l = store.lock().unwrap();
        let p = ScanPolicy::default();
        let r1 = snapshot(r, &store, &p, turn("1")).unwrap();
        assert!(!r1.reused);
        assert_eq!(r1.manifest.id, 1);
        let r2 = snapshot(r, &store, &p, pretool()).unwrap();
        assert!(
            !r2.reused,
            "PreTool after TurnStart still writes so `unwreck log` shows the tool"
        );
        assert_eq!(r2.manifest.id, 2);
        let r2r = snapshot(r, &store, &p, pretool()).unwrap();
        assert!(r2r.reused);
        assert_eq!(r2r.manifest.id, 2);
        assert!(
            matches!(r2r.manifest.trigger, Trigger::PreTool { .. }),
            "original trigger is kept"
        );
        assert_eq!(store.list().unwrap(), vec![1, 2]);
        // Older stores without tip.json still reuse via the last manifest.
        fs::remove_file(st.path().join("tip.json")).unwrap();
        let r2b = snapshot(r, &store, &p, pretool()).unwrap();
        assert!(r2b.reused);
        assert_eq!(r2b.manifest.id, 2);
        fs::write(r.join("a.rs"), "fn b(){}").unwrap();
        age(&r.join("a.rs"), 30);
        let r3 = snapshot(r, &store, &p, turn("3")).unwrap();
        assert!(!r3.reused);
        assert_eq!(r3.manifest.id, 3);
        let r4 = snapshot(
            r,
            &store,
            &p,
            Trigger::Manual {
                label: Some("pin".into()),
            },
        )
        .unwrap();
        assert!(!r4.reused, "manual pins always write");
        assert_eq!(r4.manifest.id, 4);
        let r5 = snapshot(r, &store, &p, Trigger::PreRestore { restoring: 1 }).unwrap();
        assert!(!r5.reused, "restore bookkeeping always writes");
        assert_eq!(r5.manifest.id, 5);
    }
}
