//! Pack index codec (`UWIDX` v1) and loading committed packs from `packs/`.
use crate::{Loc, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use unwreck_core::ContentHash;

const IDX_MAGIC: &[u8; 8] = b"UWIDX\x01\0\0";
const IDX_REC: usize = 32 + 8 + 4;

pub(crate) fn encode_idx(entries: &[(ContentHash, u64, u32)], pack_len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 16 + entries.len() * IDX_REC + 32);
    out.extend_from_slice(IDX_MAGIC);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    out.extend_from_slice(&pack_len.to_le_bytes());
    for (h, off, len) in entries {
        out.extend_from_slice(&h.0);
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }
    let sum = blake3::hash(&out);
    out.extend_from_slice(sum.as_bytes());
    out
}

/// Parse an index; None if it is malformed or its checksum does not match.
fn decode_idx(b: &[u8]) -> Option<Vec<(ContentHash, u64, u32)>> {
    if b.len() < 8 + 16 + 32 || &b[..8] != IDX_MAGIC {
        return None;
    }
    let (body, sum) = b.split_at(b.len() - 32);
    if blake3::hash(body).as_bytes() != sum {
        return None;
    }
    let n = u64::from_le_bytes(body[8..16].try_into().ok()?) as usize;
    if body.len() != 24 + n * IDX_REC {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for r in body[24..].as_chunks::<IDX_REC>().0 {
        let h = ContentHash(r[..32].try_into().ok()?);
        let off = u64::from_le_bytes(r[32..40].try_into().ok()?);
        let len = u32::from_le_bytes(r[40..44].try_into().ok()?);
        out.push((h, off, len));
    }
    Some(out)
}

fn pack_id(p: &Path) -> Option<u64> {
    p.file_stem()?.to_str()?.strip_prefix("pack-")?.parse().ok()
}

pub(crate) fn load_index(dir: &Path) -> Result<(HashMap<ContentHash, Loc>, u64)> {
    let mut index = HashMap::new();
    let mut max_id = 0;
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    for p in paths {
        let Some(id) = pack_id(&p) else { continue };
        max_id = max_id.max(id);
        if p.extension().is_none_or(|e| e != "idx") {
            continue;
        }
        let Ok(pack_len) = fs::metadata(p.with_extension("pack")).map(|m| m.len()) else {
            continue;
        };
        let Some(entries) = fs::read(&p).ok().and_then(|b| decode_idx(&b)) else {
            eprintln!("unwreck: ignoring unreadable pack index {}", p.display());
            continue;
        };
        for (h, off, len) in entries {
            if off + len as u64 <= pack_len {
                index.entry(h).or_insert(Loc::Pack { pack: id, off, len });
            }
        }
    }
    Ok((index, max_id + 1))
}
