//! Property test: snapshot → random edits → restore equals the original tree.
use crate::plan;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use unwreck_core::Trigger;
use unwreck_scan::{ScanPolicy, snapshot};
use unwreck_store::Store;

fn write_tree(root: &Path, files: &BTreeMap<String, String>) {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).unwrap();
    for (p, body) in files {
        if p.contains("..") || p.starts_with('/') || p.contains('\0') {
            continue;
        }
        let abs = root.join(p);
        if let Some(d) = abs.parent() {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(abs, body.as_bytes()).unwrap();
    }
}

fn read_tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            if n == ".git" {
                continue;
            }
            let r = if rel.is_empty() {
                n.to_owned()
            } else {
                format!("{rel}/{n}")
            };
            let p = e.path();
            if p.is_dir() {
                walk(&p, &r, out);
            } else if let Ok(s) = fs::read_to_string(&p) {
                out.insert(r, s);
            }
        }
    }
    walk(root, "", &mut out);
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]
    #[test]
    fn snapshot_edit_restore_roundtrip(
        files in prop::collection::btree_map("[a-z]{1,3}(/[a-z]{1,3}){0,2}\\.txt", "[ -~]{0,40}", 1..6),
        extra in "[a-z]{1,4}\\.txt",
        extra_body in "[ -~]{0,20}",
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let store = Store::open(tmp.path().join("store")).unwrap();
        write_tree(&root, &files);
        let _l = store.lock().unwrap();
        let original = snapshot(&root, &store, &ScanPolicy::default(), Trigger::Manual { label: Some("orig".into()) }).unwrap();
        let mut changed = files.clone();
        if let Some(k) = changed.keys().next().cloned() {
            changed.get_mut(&k).unwrap().push('x');
        }
        changed.insert(extra.clone(), extra_body);
        write_tree(&root, &changed);
        let last = snapshot(&root, &store, &ScanPolicy::default(), Trigger::Manual { label: Some("after".into()) }).unwrap();
        let current = snapshot(&root, &store, &ScanPolicy::default(), Trigger::PreRestore { restoring: original.manifest.id }).unwrap();
        let p = plan(&original.manifest, &last.manifest, &current.manifest, None);
        crate::apply(&root, &store, &p, true).unwrap();
        drop(_l);
        let got = read_tree(&root);
        prop_assert_eq!(got, files);
    }
}
