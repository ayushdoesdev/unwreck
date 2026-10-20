//! Content-addressed snapshot store.
//!
//! Layout (outside the worktree, see PLAN.md):
//! ```text
//! <store>/packs/pack-0000000007.pack  concatenated zstd blobs (each < PACK_THRESHOLD compressed)
//! <store>/packs/pack-0000000007.idx   BLAKE3 -> (offset, len) for that pack, checksummed
//! <store>/objects/ab/abcdef....zst    loose blob: large blobs, and everything older builds wrote
//! <store>/manifests/0000000000000042.json.zst
//! <store>/statcache.bin               owned by unwreck-scan (rebuildable cache)
//! <store>/LOCK                        std::fs::File::lock (exclusive) during writes
//! ```
//!
//! Write path: blobs `put` during a snapshot go into one *batch*: small ones are appended
//! to a new pack file, large ones to temp files. `commit()` (called by `write_manifest`)
//! fsyncs the pack once, then writes its `.idx` atomically (temp + fsync + rename), then
//! renames the loose blobs into place, then the manifest is written. So a manifest never
//! references a blob that is not durable, and a crash at any point leaves at worst an
//! orphan pack without an index (ignored, and removed by `gc`).
//!
//! Read path: a pack's index is trusted only if its checksum is valid, and only for the
//! entries that fit inside the pack's actual length. A truncated pack therefore loses just
//! the blobs past the cut; they show up as missing and the next snapshot stores them again.
#![warn(missing_docs)]

mod pack;
mod perms;

use crate::pack::{encode_idx, load_index};
use crate::perms::{chmod_file, create_private, create_private_new, ensure_private_dir, open_lock};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use unwreck_core::{
    ContentHash, Entry, EntryKind, MANIFEST_VERSION, Manifest, RelPath, Trigger, tree_fingerprint,
};

/// Failures reading or writing the snapshot store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Manifest JSON could not be decoded.
    #[error("manifest decode: {0}")]
    Json(#[from] serde_json::Error),
    /// Blob bytes did not match the content hash.
    #[error("object {0} is corrupt (hash mismatch)")]
    Corrupt(String),
    /// A referenced blob is not in packs or loose objects.
    #[error("object {0} is missing")]
    Missing(String),
    /// No manifest file for this snapshot id.
    #[error("snapshot {0} not found")]
    NoSnapshot(u64),
}
/// [`Store`] result type.
pub type Result<T> = std::result::Result<T, StoreError>;

const ZSTD_LEVEL: i32 = 3;
/// Blobs whose compressed size is below this go into packs; the rest stay loose.
pub const PACK_THRESHOLD: usize = 64 * 1024;
#[derive(Clone, Copy, Debug)]
pub(crate) enum Loc {
    /// In a committed pack.
    Pack { pack: u64, off: u64, len: u32 },
    /// In the current batch's pack (not yet indexed on disk).
    PendingPack { off: u64, len: u32 },
    /// In the current batch's temp file `batch.loose[i]`.
    PendingLoose(usize),
}

struct PackWriter {
    id: u64,
    path: PathBuf,
    w: BufWriter<File>,
    len: u64,
    entries: Vec<(ContentHash, u64, u32)>,
}

#[derive(Default)]
struct Batch {
    pack: Option<PackWriter>,
    /// (temp path, final path, hash)
    loose: Vec<(PathBuf, PathBuf, ContentHash)>,
}

struct Inner {
    index: HashMap<ContentHash, Loc>,
    next_pack: u64,
    batch: Batch,
}

/// On-disk snapshot store: packs, loose objects, manifests, and the write lock.
pub struct Store {
    root: PathBuf,
    inner: Mutex<Inner>,
}

/// Exclusive lock held for the duration of a write transaction.
pub struct StoreLock(File);
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

const TIP: &str = "tip.json";

/// Compact identity of the latest snapshot, so an unchanged scan can skip reading the
/// full manifest. Missing or corrupt `tip.json` falls back to `latest()`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotTip {
    /// Snapshot id of the latest written (or reused) manifest.
    pub id: u64,
    /// Timestamp copied from that manifest.
    pub created_unix_ms: u64,
    /// Trigger of the original manifest (not the reused no-op trigger).
    pub trigger: Trigger,
    /// [`unwreck_core::tree_fingerprint`] of that tree.
    pub tree: ContentHash,
}

impl SnapshotTip {
    fn from_manifest(m: &Manifest) -> Self {
        Self {
            id: m.id,
            created_unix_ms: m.created_unix_ms,
            trigger: m.trigger.clone(),
            tree: m.tree_fingerprint(),
        }
    }
}

/// Counts reported by `gc`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Manifests deleted because they were not in the keep set.
    pub manifests_removed: usize,
    /// Unreferenced loose objects deleted.
    pub loose_removed: usize,
    /// Packed blobs not referenced by kept manifests (dropped during repack).
    pub packed_removed: usize,
    /// Pack indexes present before gc.
    pub packs_before: usize,
    /// Pack indexes present after gc.
    pub packs_after: usize,
}

impl Store {
    /// Create the store directories if needed and load committed pack indexes.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        ensure_private_dir(&root)?;
        for d in ["objects", "manifests", "tmp", "packs"] {
            ensure_private_dir(&root.join(d))?;
        }
        let (index, next_pack) = load_index(&root.join("packs"))?;
        Ok(Self {
            root,
            inner: Mutex::new(Inner {
                index,
                next_pack,
                batch: Batch::default(),
            }),
        })
    }

    /// Filesystem root of this store (outside the worktree).
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take the cross-process write lock, then reload the pack index so blobs written by
    /// other processes since `open` are visible (and pack ids don't collide).
    pub fn lock(&self) -> Result<StoreLock> {
        let f = open_lock(&self.root.join("LOCK"))?;
        crate::perms::chmod_file(&self.root.join("LOCK")).ok();
        f.lock()?;
        let mut inner = self.inner();
        if inner.batch.pack.is_none() && inner.batch.loose.is_empty() {
            let (index, next_pack) = load_index(&self.root.join("packs"))?;
            inner.index = index;
            inner.next_pack = next_pack;
        }
        Ok(StoreLock(f))
    }

    fn object_path(&self, h: &ContentHash) -> PathBuf {
        let hex = h.to_hex();
        self.root
            .join("objects")
            .join(&hex[..2])
            .join(format!("{hex}.zst"))
    }

    fn pack_path(&self, id: u64, ext: &str) -> PathBuf {
        self.root.join("packs").join(format!("pack-{id:010}.{ext}"))
    }

    /// BLAKE3 of `bytes` (the object name).
    pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash(*blake3::hash(bytes).as_bytes())
    }

    /// True if this hash is in a pack index or as a loose object file.
    pub fn has(&self, h: &ContentHash) -> bool {
        self.inner().index.contains_key(h) || self.object_path(h).exists()
    }

    /// Hashes known to the in-memory pack index (not loose objects). Callers use this
    /// to avoid taking the store lock on every stat-cache hit.
    pub fn packed_hashes(&self) -> std::collections::HashSet<ContentHash> {
        self.inner().index.keys().copied().collect()
    }

    /// Store bytes; no-op if an identical blob already exists (dedup). The blob becomes
    /// durable at the next `commit()` / `write_manifest`. Caller must hold `lock()`.
    pub fn put(&self, bytes: &[u8]) -> Result<ContentHash> {
        let h = Self::hash_bytes(bytes);
        self.put_hashed(h, bytes)?;
        Ok(h)
    }

    /// Like `put` when the caller already computed the BLAKE3 hash. Returns true if new.
    pub fn put_hashed(&self, h: ContentHash, bytes: &[u8]) -> Result<bool> {
        if self.has(&h) {
            return Ok(false);
        }
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL)?;
        self.put_compressed(h, &compressed)?;
        Ok(true)
    }

    fn put_compressed(&self, h: ContentHash, compressed: &[u8]) -> Result<()> {
        let mut guard = self.inner();
        let inner = &mut *guard;
        if compressed.len() < PACK_THRESHOLD {
            if inner.batch.pack.is_none() {
                let id = inner.next_pack;
                inner.next_pack += 1;
                let path = self.pack_path(id, "pack");
                let f = create_private(&path)?;
                inner.batch.pack = Some(PackWriter {
                    id,
                    path,
                    w: BufWriter::with_capacity(1 << 20, f),
                    len: 0,
                    entries: Vec::new(),
                });
            }
            let pw = inner.batch.pack.as_mut().expect("pack writer");
            let off = pw.len;
            let len = compressed.len() as u32;
            pw.w.write_all(compressed)?;
            pw.len += len as u64;
            pw.entries.push((h, off, len));
            inner.index.insert(h, Loc::PendingPack { off, len });
        } else {
            let (tmp, mut f) = tempfile_in(&self.root.join("tmp"))?;
            f.write_all(compressed)?;
            inner.batch.loose.push((tmp, self.object_path(&h), h));
            inner
                .index
                .insert(h, Loc::PendingLoose(inner.batch.loose.len() - 1));
        }
        Ok(())
    }

    fn read_compressed(&self, h: &ContentHash) -> Result<Vec<u8>> {
        let mut inner = self.inner();
        let loc = inner.index.get(h).copied();
        let read_at = |path: &Path, off: u64, len: u32| -> Result<Vec<u8>> {
            let mut f = File::open(path)?;
            f.seek(SeekFrom::Start(off))?;
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf)?;
            Ok(buf)
        };
        match loc {
            Some(Loc::Pack { pack, off, len }) => read_at(&self.pack_path(pack, "pack"), off, len),
            Some(Loc::PendingPack { off, len }) => {
                let pw = inner.batch.pack.as_mut().expect("pending pack");
                pw.w.flush()?;
                read_at(&pw.path, off, len)
            }
            Some(Loc::PendingLoose(i)) => Ok(fs::read(&inner.batch.loose[i].0)?),
            None => {
                drop(inner);
                match fs::read(self.object_path(h)) {
                    Ok(b) => Ok(b),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(StoreError::Missing(h.to_hex()))
                    }
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Decompress a blob and verify its BLAKE3 hash.
    pub fn get(&self, h: &ContentHash) -> Result<Vec<u8>> {
        let compressed = self.read_compressed(h)?;
        let bytes = zstd::stream::decode_all(&compressed[..])
            .map_err(|_| StoreError::Corrupt(h.to_hex()))?;
        if Self::hash_bytes(&bytes) != *h {
            return Err(StoreError::Corrupt(h.to_hex()));
        }
        Ok(bytes)
    }

    /// Make every blob `put` since the last commit durable: one fsync for the pack, one for
    /// its index, one per large loose blob, plus directory fsyncs. Caller must hold `lock()`.
    pub fn commit(&self) -> Result<()> {
        let mut inner = self.inner();
        let batch = std::mem::take(&mut inner.batch);
        if let Some(pw) = batch.pack {
            let f = pw.w.into_inner().map_err(|e| e.into_error())?;
            f.sync_data()?;
            drop(f);
            let idx = encode_idx(&pw.entries, pw.len);
            self.atomic_write(&self.pack_path(pw.id, "idx"), &idx, true)?;
            for (h, off, len) in pw.entries {
                inner.index.insert(
                    h,
                    Loc::Pack {
                        pack: pw.id,
                        off,
                        len,
                    },
                );
            }
        }
        let mut dirs = HashSet::new();
        for (tmp, dest, h) in batch.loose {
            File::open(&tmp)?.sync_all()?;
            let parent = dest.parent().expect("object has parent").to_path_buf();
            ensure_private_dir(&parent)?;
            fs::rename(&tmp, &dest)?;
            chmod_file(&dest).ok();
            inner.index.remove(&h); // loose objects are found by path
            dirs.insert(parent);
        }
        for d in dirs {
            sync_dir(&d)?;
        }
        Ok(())
    }

    /// Write to tmp/ then rename, so a crash never leaves a half-written file.
    fn atomic_write(&self, dest: &Path, data: &[u8], durable: bool) -> Result<()> {
        let (tmp, mut f) = tempfile_in(&self.root.join("tmp"))?;
        f.write_all(data)?;
        if durable {
            f.sync_all()?;
        }
        drop(f);
        fs::rename(&tmp, dest)?;
        chmod_file(dest).ok();
        if durable {
            sync_dir(dest.parent().expect("dest has parent"))?;
        }
        Ok(())
    }

    /// Warnings if the store is looser than 0700/0600. `unwreck doctor` prints these.
    pub fn permission_warnings(&self) -> Vec<String> {
        crate::perms::permission_warnings(&self.root)
    }

    /// Atomically replace an auxiliary file in the store root (e.g. the stat cache).
    /// Not fsynced: callers must checksum the contents and treat a bad file as absent.
    pub fn write_aux(&self, name: &str, data: &[u8]) -> Result<()> {
        self.atomic_write(&self.root.join(name), data, false)
    }

    /// Read an auxiliary file from the store root, or `None` if missing.
    pub fn read_aux(&self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.root.join(name)).ok()
    }

    /// Sorted snapshot ids currently on disk.
    pub fn list(&self) -> Result<Vec<u64>> {
        let mut ids = Vec::new();
        for e in fs::read_dir(self.root.join("manifests"))? {
            let name = e?.file_name();
            if let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json.zst"))
                .and_then(|n| n.parse().ok())
            {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// The newest snapshot, if any.
    pub fn latest(&self) -> Result<Option<Manifest>> {
        match self.list()?.last() {
            Some(&id) => Ok(Some(self.read_manifest(id)?)),
            None => Ok(None),
        }
    }

    fn manifest_path(&self, id: u64) -> PathBuf {
        self.root
            .join("manifests")
            .join(format!("{id:016}.json.zst"))
    }

    /// Commits pending blobs, then writes the manifest. Caller must hold `lock()`.
    /// Entries and empty dirs are sorted here.
    pub fn write_manifest(
        &self,
        trigger: Trigger,
        mut entries: Vec<Entry>,
        mut empty_dirs: Vec<RelPath>,
    ) -> Result<Manifest> {
        self.commit()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        empty_dirs.sort();
        empty_dirs.dedup();
        let id = self.list()?.last().map_or(1, |l| l + 1);
        let created_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let m = Manifest {
            version: MANIFEST_VERSION,
            id,
            created_unix_ms,
            trigger,
            entries,
            empty_dirs,
        };
        let json = serde_json::to_vec(&m)?;
        let compressed = zstd::bulk::compress(&json, ZSTD_LEVEL)?;
        // Test-only crash point: blobs are durable, manifest is not. Like kill -9 mid-snap.
        if std::env::var_os("UNWRECK_INJECT_CRASH").is_some_and(|v| v == "after_blobs") {
            std::process::abort();
        }
        self.atomic_write(&self.manifest_path(id), &compressed, true)?;
        let tip = SnapshotTip::from_manifest(&m);
        self.write_aux(TIP, &serde_json::to_vec(&tip)?)?;
        Ok(m)
    }

    /// Latest snapshot id/trigger/tree hash. Prefer `tip.json`; reconstruct from the last
    /// manifest if an older store has no tip file.
    pub fn tip(&self) -> Result<Option<SnapshotTip>> {
        if let Some(b) = self.read_aux(TIP)
            && let Ok(t) = serde_json::from_slice::<SnapshotTip>(&b)
        {
            return Ok(Some(t));
        }
        Ok(self.latest()?.as_ref().map(SnapshotTip::from_manifest))
    }

    /// Rebuild a full manifest for a reused no-op snapshot from a just-scanned tree.
    pub fn reused_manifest(
        &self,
        tip: &SnapshotTip,
        entries: Vec<Entry>,
        empty_dirs: Vec<RelPath>,
    ) -> Manifest {
        debug_assert_eq!(tree_fingerprint(&entries, &empty_dirs), tip.tree);
        Manifest {
            version: MANIFEST_VERSION,
            id: tip.id,
            created_unix_ms: tip.created_unix_ms,
            trigger: tip.trigger.clone(),
            entries,
            empty_dirs,
        }
    }

    /// Load and decompress one snapshot manifest.
    pub fn read_manifest(&self, id: u64) -> Result<Manifest> {
        let path = self.manifest_path(id);
        if !path.exists() {
            return Err(StoreError::NoSnapshot(id));
        }
        let bytes = zstd::stream::decode_all(File::open(path)?)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Mark-and-sweep plus repack. Deletes manifests not in `keep`, loose objects no kept
    /// manifest references, then copies every live packed blob into one new pack (raw
    /// compressed bytes, no recompression), commits it, and only then deletes the old packs.
    /// A crash mid-gc leaves duplicates (harmless), never a missing live blob.
    /// Caller must hold `lock()`.
    pub fn gc(&self, keep: &[u64]) -> Result<GcReport> {
        self.commit()?;
        let mut rep = GcReport::default();
        let mut live = HashSet::new();
        for id in self.list()? {
            if keep.contains(&id) {
                for e in self.read_manifest(id)?.entries {
                    if let EntryKind::File { hash, .. } = e.kind {
                        live.insert(hash);
                    }
                }
            } else {
                fs::remove_file(self.manifest_path(id))?;
                rep.manifests_removed += 1;
            }
        }
        let live_hex: HashSet<String> = live.iter().map(|h| h.to_hex()).collect();
        for shard in fs::read_dir(self.root.join("objects"))? {
            for obj in fs::read_dir(shard?.path())? {
                let obj = obj?;
                let name = obj.file_name();
                let hex = name
                    .to_str()
                    .and_then(|n| n.strip_suffix(".zst"))
                    .unwrap_or_default();
                if !live_hex.contains(hex) {
                    fs::remove_file(obj.path())?;
                    rep.loose_removed += 1;
                }
            }
        }

        self.gc_packs(&live, &mut rep)?;
        for e in fs::read_dir(self.root.join("tmp"))?.flatten() {
            let _ = fs::remove_file(e.path());
        }
        match self.latest()? {
            Some(m) => {
                self.write_aux(TIP, &serde_json::to_vec(&SnapshotTip::from_manifest(&m))?)?
            }
            None => {
                let _ = fs::remove_file(self.root.join(TIP));
            }
        }
        Ok(rep)
    }

    /// Drop unreferenced pack blobs. Rewrite a pack file only when dead compressed
    /// bytes exceed [`unwreck_core::config::DEFAULT_REPACK_DEAD_RATIO`] of the file.
    fn gc_packs(&self, live: &HashSet<ContentHash>, rep: &mut GcReport) -> Result<()> {
        let packs_dir = self.root.join("packs");
        let files: Vec<PathBuf> = fs::read_dir(&packs_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        // Orphan pack bodies (no idx) are never referenced; delete them.
        for p in &files {
            if p.extension().is_some_and(|e| e == "pack") && !p.with_extension("idx").exists() {
                let _ = fs::remove_file(p);
            }
        }
        let packed: Vec<(ContentHash, Loc)> = {
            let inner = self.inner();
            inner.index.iter().map(|(h, l)| (*h, *l)).collect()
        };
        let mut by_pack: BTreeMap<u64, Vec<(ContentHash, u64, u32, bool)>> = BTreeMap::new();
        for (h, l) in packed {
            if let Loc::Pack { pack, off, len } = l {
                let is_live = live.contains(&h);
                if !is_live {
                    rep.packed_removed += 1;
                }
                by_pack
                    .entry(pack)
                    .or_default()
                    .push((h, off, len, is_live));
            }
        }
        rep.packs_before = by_pack.len();
        {
            self.inner()
                .index
                .retain(|_, loc| !matches!(loc, Loc::Pack { .. }));
        }
        const RATIO: f64 = unwreck_core::config::DEFAULT_REPACK_DEAD_RATIO;
        for (pack, items) in by_pack {
            let live_items: Vec<(ContentHash, u64, u32)> = items
                .iter()
                .filter(|i| i.3)
                .map(|i| (i.0, i.1, i.2))
                .collect();
            let pack_path = self.pack_path(pack, "pack");
            let idx_path = self.pack_path(pack, "idx");
            if live_items.is_empty() {
                let _ = fs::remove_file(&idx_path);
                let _ = fs::remove_file(&pack_path);
                continue;
            }
            let file_len = fs::metadata(&pack_path).map(|m| m.len()).unwrap_or(1);
            let live_bytes: u64 = live_items.iter().map(|i| i.2 as u64).sum();
            let dead_ratio = 1.0 - (live_bytes as f64 / file_len.max(1) as f64);
            if dead_ratio > RATIO {
                let mut f = File::open(&pack_path)?;
                let mut sorted = live_items.clone();
                sorted.sort_by_key(|i| i.1);
                for (h, off, len) in sorted {
                    let mut buf = vec![0u8; len as usize];
                    f.seek(SeekFrom::Start(off))?;
                    f.read_exact(&mut buf)?;
                    if !self.inner().index.contains_key(&h) {
                        self.put_compressed(h, &buf)?;
                    }
                }
                let _ = fs::remove_file(&idx_path);
                let _ = fs::remove_file(&pack_path);
            } else {
                if items.iter().any(|i| !i.3) {
                    self.atomic_write(&idx_path, &encode_idx(&live_items, file_len), true)?;
                }
                let mut inner = self.inner();
                for (h, off, len) in live_items {
                    inner.index.insert(h, Loc::Pack { pack, off, len });
                }
            }
        }
        self.commit()?;
        sync_dir(&packs_dir)?;
        let idxs = fs::read_dir(&packs_dir)?
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "idx"))
            .count();
        rep.packs_after = idxs;
        Ok(())
    }

    /// (number of committed packs, total pack bytes, number of loose objects)
    pub fn stats(&self) -> Result<(usize, u64, usize)> {
        let mut packs = 0;
        let mut bytes = 0;
        for e in fs::read_dir(self.root.join("packs"))?.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "pack") && p.with_extension("idx").exists() {
                packs += 1;
                bytes += e.metadata()?.len();
            }
        }
        let mut loose = 0;
        for shard in fs::read_dir(self.root.join("objects"))?.flatten() {
            loose += fs::read_dir(shard.path())?.count();
        }
        Ok((packs, bytes, loose))
    }

    /// Recursive byte size of the store directory (does not follow symlinks).
    pub fn du_bytes(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            let meta = match fs::symlink_metadata(p) {
                Ok(m) => m,
                Err(_) => return 0,
            };
            if meta.is_dir() {
                fs::read_dir(p)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| walk(&e.path()))
                    .sum()
            } else {
                meta.len()
            }
        }
        walk(&self.root)
    }
}

impl Drop for Store {
    /// An uncommitted batch (e.g. an error mid-snapshot) leaves files nobody indexes;
    /// remove them best-effort. A hard crash leaves them too; `gc` cleans those up.
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(|e| e.into_inner());
        let batch = std::mem::take(&mut inner.batch);
        if let Some(pw) = batch.pack {
            drop(pw.w);
            let _ = fs::remove_file(&pw.path);
        }
        for (tmp, _, _) in batch.loose {
            let _ = fs::remove_file(tmp);
        }
    }
}

#[cfg(unix)]
fn sync_dir(d: &Path) -> Result<()> {
    File::open(d)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_d: &Path) -> Result<()> {
    Ok(())
}

/// Minimal unique temp file without an extra dependency.
fn tempfile_in(dir: &Path) -> Result<(PathBuf, File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let p = dir.join(format!("{}-{nanos}-{n}.tmp", std::process::id()));
    let f = create_private_new(&p)?;
    Ok((p, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, h: ContentHash) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File {
                hash: h,
                size: 0,
                executable: false,
            },
        }
    }
    fn manual() -> Trigger {
        Trigger::Manual { label: None }
    }
    fn packs(dir: &Path, ext: &str) -> Vec<PathBuf> {
        let mut v: Vec<_> = fs::read_dir(dir.join("packs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .collect();
        v.sort();
        v
    }
    /// Incompressible bytes so the large blob really exceeds PACK_THRESHOLD.
    fn noise(n: usize, seed: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(n + 32);
        let mut h = blake3::hash(&[seed]);
        while out.len() < n {
            out.extend_from_slice(h.as_bytes());
            h = blake3::hash(h.as_bytes());
        }
        out.truncate(n);
        out
    }

    #[test]
    fn put_get_dedup_and_gc() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        let h1 = s.put(b"hello").unwrap();
        let h2 = s.put(b"hello").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(s.get(&h1).unwrap(), b"hello"); // readable before commit
        let m1 = s
            .write_manifest(manual(), vec![file("a.txt", h1)], vec![])
            .unwrap();
        let m2 = s.write_manifest(manual(), vec![], vec![]).unwrap();
        assert_eq!((m1.id, m2.id), (1, 2));
        let rep = s.gc(&[2]).unwrap();
        assert_eq!((rep.manifests_removed, rep.packed_removed), (1, 1));
        assert!(!s.has(&h1));
    }

    #[test]
    fn small_blobs_share_one_pack_large_blob_is_loose() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        let small: Vec<_> = (0..500u32)
            .map(|i| s.put(format!("file {i}").as_bytes()).unwrap())
            .collect();
        let big_bytes = noise(200_000, 1);
        let big = s.put(&big_bytes).unwrap();
        s.write_manifest(manual(), vec![], vec![]).unwrap();
        assert_eq!(packs(dir.path(), "pack").len(), 1);
        assert_eq!(packs(dir.path(), "idx").len(), 1);
        assert!(s.object_path(&big).exists());
        drop(_l);
        drop(s);
        let s = Store::open(dir.path()).unwrap(); // fresh process view
        assert_eq!(s.get(&small[123]).unwrap(), b"file 123");
        assert_eq!(s.get(&big).unwrap(), big_bytes);
        assert_eq!(
            s.stats().unwrap(),
            (
                1,
                fs::metadata(&packs(dir.path(), "pack")[0]).unwrap().len(),
                1
            )
        );
    }

    #[test]
    fn dedup_across_snapshots_adds_no_new_pack() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        {
            let _l = s.lock().unwrap();
            let h = s.put(b"same").unwrap();
            s.write_manifest(manual(), vec![file("a", h)], vec![])
                .unwrap();
        }
        let s2 = Store::open(dir.path()).unwrap();
        let _l = s2.lock().unwrap();
        assert!(!s2.put_hashed(Store::hash_bytes(b"same"), b"same").unwrap());
        s2.write_manifest(
            manual(),
            vec![file("a", Store::hash_bytes(b"same"))],
            vec![],
        )
        .unwrap();
        assert_eq!(packs(dir.path(), "pack").len(), 1);
    }

    #[test]
    fn crash_before_commit_leaves_no_index_and_store_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let committed;
        {
            let s = Store::open(dir.path()).unwrap();
            let _l = s.lock().unwrap();
            committed = s.put(b"committed").unwrap();
            s.write_manifest(manual(), vec![file("a", committed)], vec![])
                .unwrap();
            s.put(b"lost in crash").unwrap();
            {
                let mut inner = s.inner();
                inner.batch.pack.as_mut().unwrap().w.flush().unwrap();
            }
            std::mem::forget(s); // simulate a crash: no commit, no Drop cleanup
        }
        assert_eq!(packs(dir.path(), "pack").len(), 2); // one committed, one orphan
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        assert!(!s.has(&Store::hash_bytes(b"lost in crash")));
        assert_eq!(s.get(&committed).unwrap(), b"committed");
        // Next snapshot stores it again; gc removes the orphan and repacks into one pack.
        let h = s.put(b"lost in crash").unwrap();
        let m = s
            .write_manifest(manual(), vec![file("a", committed), file("b", h)], vec![])
            .unwrap();
        s.gc(&[m.id]).unwrap();
        // Live packs with no dead data are left in place (no merge).
        assert!(!packs(dir.path(), "pack").is_empty());
        assert_eq!(
            packs(dir.path(), "pack").len(),
            packs(dir.path(), "idx").len()
        );
        assert_eq!(s.get(&h).unwrap(), b"lost in crash");
        assert_eq!(s.get(&committed).unwrap(), b"committed");
    }

    #[test]
    fn truncated_pack_drops_only_the_cut_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b);
        {
            let s = Store::open(dir.path()).unwrap();
            let _l = s.lock().unwrap();
            a = s.put(b"first blob").unwrap();
            b = s.put(b"second blob").unwrap();
            s.write_manifest(manual(), vec![file("a", a), file("b", b)], vec![])
                .unwrap();
        }
        let pack = packs(dir.path(), "pack").remove(0);
        let len = fs::metadata(&pack).unwrap().len();
        File::options()
            .write(true)
            .open(&pack)
            .unwrap()
            .set_len(len - 3)
            .unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.get(&a).unwrap(), b"first blob");
        assert!(!s.has(&b));
        assert!(matches!(s.get(&b), Err(StoreError::Missing(_))));
        let _l = s.lock().unwrap();
        assert!(s.put_hashed(b, b"second blob").unwrap()); // re-stored by the next snapshot
        s.commit().unwrap();
        assert_eq!(s.get(&b).unwrap(), b"second blob");
    }

    #[test]
    fn trailing_garbage_and_bad_index_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let a;
        {
            let s = Store::open(dir.path()).unwrap();
            let _l = s.lock().unwrap();
            a = s.put(b"keep me").unwrap();
            s.write_manifest(manual(), vec![file("a", a)], vec![])
                .unwrap();
        }
        let pack = packs(dir.path(), "pack").remove(0);
        File::options()
            .append(true)
            .open(&pack)
            .unwrap()
            .write_all(b"half-written junk")
            .unwrap();
        assert_eq!(
            Store::open(dir.path()).unwrap().get(&a).unwrap(),
            b"keep me"
        );
        // Corrupt the index: its checksum fails, so the pack's blobs read as missing (not wrong).
        let idx = packs(dir.path(), "idx").remove(0);
        let mut bytes = fs::read(&idx).unwrap();
        bytes[30] ^= 0xff;
        fs::write(&idx, bytes).unwrap();
        assert!(!Store::open(dir.path()).unwrap().has(&a));
    }

    #[test]
    fn gc_drops_dead_packs_and_leaves_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        let mut ids = vec![];
        let mut hashes = vec![];
        for i in 0..5u8 {
            let h = s.put(&[i; 100]).unwrap();
            hashes.push(h);
            ids.push(
                s.write_manifest(manual(), vec![file("x", h)], vec![])
                    .unwrap()
                    .id,
            );
        }
        assert_eq!(packs(dir.path(), "pack").len(), 5);
        let rep = s.gc(&ids[3..]).unwrap();
        assert_eq!(rep.manifests_removed, 3);
        assert_eq!(rep.packed_removed, 3);
        assert_eq!(rep.packs_before, 5);
        assert_eq!(
            rep.packs_after, 2,
            "100% dead packs deleted; live packs stay"
        );
        assert_eq!(packs(dir.path(), "pack").len(), 2);
        drop(_l);
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.get(&hashes[3]).unwrap(), vec![3u8; 100]);
        assert_eq!(s.get(&hashes[4]).unwrap(), vec![4u8; 100]);
        assert!(!s.has(&hashes[0]));
    }

    #[test]
    fn gc_repacks_only_when_more_than_30_percent_dead() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        let hashes: Vec<_> = (0..10u8).map(|i| s.put(&noise(800, i)).unwrap()).collect();
        let files: Vec<_> = hashes
            .iter()
            .enumerate()
            .map(|(i, h)| file(&format!("f{i}"), *h))
            .collect();
        let m1 = s.write_manifest(manual(), files.clone(), vec![]).unwrap();
        let m2 = s
            .write_manifest(manual(), files[2..].to_vec(), vec![])
            .unwrap();
        let _ = m1;
        let before = packs(dir.path(), "pack")[0].clone();
        let before_len = fs::metadata(&before).unwrap().len();
        // 2/10 dead ≈ 20% < 30%: idx may shrink, pack body stays.
        s.gc(&[m2.id]).unwrap();
        let after = packs(dir.path(), "pack");
        assert_eq!(after.len(), 1);
        assert_eq!(fs::metadata(&after[0]).unwrap().len(), before_len);
        assert!(s.has(&hashes[2]));
        assert!(!s.has(&hashes[0]));
    }

    #[test]
    fn legacy_loose_objects_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let h = Store::hash_bytes(b"old");
        let p = s.object_path(&h);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, zstd::bulk::compress(b"old", 3).unwrap()).unwrap();
        assert!(s.has(&h));
        assert_eq!(s.get(&h).unwrap(), b"old");
    }

    #[cfg(unix)]
    #[test]
    fn store_is_0700_and_files_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let _l = s.lock().unwrap();
        let h = s.put(b"secret").unwrap();
        s.write_manifest(manual(), vec![file("a.txt", h)], vec![])
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.path()), 0o700);
        for d in ["objects", "manifests", "tmp", "packs"] {
            assert_eq!(mode(&dir.path().join(d)), 0o700, "{d}");
        }
        assert_eq!(mode(&dir.path().join("LOCK")), 0o600);
        assert_eq!(mode(&dir.path().join("tip.json")), 0o600);
        assert!(
            s.permission_warnings().is_empty(),
            "{:?}",
            s.permission_warnings()
        );
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            s.permission_warnings().iter().any(|w| w.contains("0755")),
            "{:?}",
            s.permission_warnings()
        );
    }
}
