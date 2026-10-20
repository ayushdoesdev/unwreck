//! kill -9 analogue: abort after blobs are committed, before the manifest is written.
use std::process::Command;

#[test]
fn abort_after_blobs_leaves_a_usable_store() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), b"fn a(){}").unwrap();
    let store = tmp.path().join("store");
    let bin = env!("CARGO_BIN_EXE_unwreck");
    let crashed = Command::new(bin)
        .current_dir(&repo)
        .env("UNWRECK_DIR", &store)
        .env("UNWRECK_INJECT_CRASH", "after_blobs")
        .args(["snap", "-m", "crash"])
        .status()
        .unwrap();
    assert!(!crashed.success());
    let ok = Command::new(bin)
        .current_dir(&repo)
        .env("UNWRECK_DIR", &store)
        .args(["snap", "-m", "ok"])
        .status()
        .unwrap();
    assert!(
        ok.success(),
        "store must open and snap after a mid-write crash"
    );
}
