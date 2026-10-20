//! CLI contract: hooks exit 0 with empty stdout even on bad JSON.
use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_unwreck"))
}

#[test]
fn hook_malformed_exits_zero_empty_stdout() {
    let mut child = bin()
        .args(["hook", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "stdout must stay empty for agents");
}

#[test]
fn hook_docs_fixture_exits_zero_empty_stdout() {
    let payload = include_str!("../../unwreck-hooks/fixtures/docs/claude-UserPromptSubmit.json");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), b"fn a(){}").unwrap();
    let mut child = bin()
        .current_dir(&repo)
        .env("UNWRECK_DIR", tmp.path().join("store"))
        .args(["hook", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn run_wraps_a_command_without_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), b"fn a(){}").unwrap();
    let st = bin()
        .current_dir(&repo)
        .env("UNWRECK_DIR", tmp.path().join("store"))
        .args([
            "run",
            "--quiet-ms",
            "50",
            "--",
            "sh",
            "-c",
            "echo hi > out.txt",
        ])
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        std::fs::read_to_string(repo.join("out.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
}

fn hook_json(agent: &str, repo: &std::path::Path, store: &std::path::Path, json: &str) {
    let mut child = bin()
        .current_dir(repo)
        .env("UNWRECK_DIR", store)
        .args(["hook", agent])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn hook_codex_shell_pretool_fixture_exits_zero_empty_stdout() {
    let payload = include_str!("../../unwreck-hooks/fixtures/docs/codex-PreToolUse-Bash.json");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), b"fn a(){}").unwrap();
    hook_json("codex", &repo, &tmp.path().join("store"), payload);
}

#[test]
fn diff_no_args_is_last_turn_start_vs_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), b"old").unwrap();
    hook_json(
        "claude",
        &repo,
        &store,
        r#"{"hook_event_name":"UserPromptSubmit","prompt_id":"p1","prompt":"go"}"#,
    );
    std::fs::write(repo.join("src/a.rs"), b"new").unwrap();
    std::fs::write(repo.join("src/b.rs"), b"added").unwrap();
    hook_json(
        "claude",
        &repo,
        &store,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo"}}"#,
    );
    let out = bin()
        .current_dir(&repo)
        .env("UNWRECK_DIR", &store)
        .arg("diff")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("src/a.rs"), "{text}");
    assert!(text.contains("src/b.rs"), "{text}");
    let out_n = bin()
        .current_dir(&repo)
        .env("UNWRECK_DIR", &store)
        .args(["diff", "1"])
        .output()
        .unwrap();
    assert_eq!(out_n.stdout, out.stdout);
    let out_nm = bin()
        .current_dir(&repo)
        .env("UNWRECK_DIR", &store)
        .args(["diff", "1", "2"])
        .output()
        .unwrap();
    assert!(out_nm.status.success());
    let nm = String::from_utf8_lossy(&out_nm.stdout);
    assert!(nm.contains("src/a.rs") || nm.contains("src/b.rs"), "{nm}");
    let help = bin().args(["diff", "--help"]).output().unwrap();
    let h = String::from_utf8_lossy(&help.stdout);
    assert!(h.contains("last turn-start"), "{h}");
    assert!(h.contains("unwreck diff 12"), "{h}");
}
