//! Unified diffs of snapshot file contents (`similar`).
use similar::{ChangeTag, TextDiff};
use unwreck_core::{EntryKind, Manifest};
use unwreck_store::Store;

pub fn unified(store: &Store, old: &Manifest, new: &Manifest, path: &str) -> String {
    let a = file_text(store, old, path);
    let b = file_text(store, new, path);
    match (a.as_deref(), b.as_deref()) {
        (Some("<binary>"), _) | (_, Some("<binary>")) => {
            format!("--- a/{path}\n+++ b/{path}\nbinary file\n")
        }
        (None, None) => String::new(),
        _ => {
            let left = a.as_deref().unwrap_or("");
            let right = b.as_deref().unwrap_or("");
            let diff = TextDiff::from_lines(left, right);
            let mut out = format!("--- a/{path}\n+++ b/{path}\n");
            for op in diff.ops() {
                for change in diff.iter_changes(op) {
                    let sign = match change.tag() {
                        ChangeTag::Delete => '-',
                        ChangeTag::Insert => '+',
                        ChangeTag::Equal => ' ',
                    };
                    out.push(sign);
                    out.push_str(change.value());
                    if !change.value().ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            out
        }
    }
}

fn file_text(store: &Store, m: &Manifest, path: &str) -> Option<String> {
    match m.get(path).map(|e| &e.kind) {
        Some(EntryKind::File { hash, .. }) => match store.get(hash) {
            Ok(b) => match String::from_utf8(b) {
                Ok(s) => Some(s),
                Err(_) => Some("<binary>".into()),
            },
            Err(_) => Some("<missing from store>".into()),
        },
        Some(EntryKind::Symlink { target }) => Some(format!("symlink → {target}\n")),
        Some(EntryKind::Oversize { size }) => Some(format!("oversize ({size} bytes)\n")),
        None => None,
    }
}
