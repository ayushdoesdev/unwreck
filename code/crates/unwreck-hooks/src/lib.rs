//! Parse agent hook payloads (JSON on stdin) into snapshot triggers, and emit the
//! config snippets that register `unwreck hook AGENT` with each agent.
//!
//! Event names verified against vendor docs on 2026-10-18 (see PLAN.md "Hook support"):
//! * Claude Code: UserPromptSubmit, PreToolUse, Stop (also SessionStart, PostToolUse, ...)
//! * Codex:       UserPromptSubmit, PreToolUse, Stop (payload adds `turn_id`)
//! * Gemini CLI:  BeforeAgent, BeforeTool, AfterAgent
#![warn(missing_docs)]

use serde::Deserialize;

pub mod install;

/// Supported agent hook dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code (`UserPromptSubmit` / `PreToolUse` / `Stop`).
    Claude,
    /// Codex (`turn_id` in payloads).
    Codex,
    /// Gemini CLI (`BeforeAgent` / `BeforeTool` / `AfterAgent`).
    Gemini,
}

impl Agent {
    /// All shipped hook dialects, in CLI order.
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Gemini];

    /// Lowercase name used in `unwreck hook AGENT` and log labels.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
        }
    }
}

/// Subset of vendor hook JSON fields we care about. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
pub struct HookPayload {
    /// Event name (`UserPromptSubmit`, `PreToolUse`, `BeforeAgent`, …).
    pub hook_event_name: String,
    /// Claude/Codex session id.
    pub session_id: Option<String>,
    /// Codex turn id.
    pub turn_id: Option<String>,
    /// Claude prompt id (used as turn key when `turn_id` is absent).
    pub prompt_id: Option<String>,
    /// Working directory from the payload, if any.
    pub cwd: Option<String>,
    /// User prompt text, if present.
    pub prompt: Option<String>,
    /// Tool name on tool events.
    pub tool_name: Option<String>,
    /// Tool arguments (we read `command` or `file_path`).
    pub tool_input: Option<serde_json::Value>,
}

/// Normalized event after parsing a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// Start of a turn.
    TurnStart {
        /// Prompt text, if the vendor included it.
        prompt: Option<String>,
    },
    /// Immediately before a tool call.
    PreTool {
        /// Tool name.
        tool: String,
        /// Command or file path, if present.
        detail: Option<String>,
    },
    /// End of a turn.
    TurnEnd,
    /// Events we do not snapshot on.
    Ignore,
}

/// Payload was not JSON.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// `serde_json` parse error.
    #[error("hook payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Parse stdin JSON into a [`HookPayload`]. Extra fields are ignored.
pub fn parse(raw: &str) -> Result<HookPayload, HookError> {
    Ok(serde_json::from_str(raw)?)
}

impl HookPayload {
    /// Codex `turn_id`, else Claude `prompt_id`.
    pub fn turn_key(&self) -> Option<String> {
        self.turn_id.clone().or_else(|| self.prompt_id.clone())
    }

    /// Map the vendor event name onto [`HookEvent`]. Unknown names are [`HookEvent::Ignore`].
    pub fn event(&self) -> HookEvent {
        match self.hook_event_name.as_str() {
            "UserPromptSubmit" | "BeforeAgent" => HookEvent::TurnStart {
                prompt: self.prompt.clone(),
            },
            "PreToolUse" | "BeforeTool" => {
                let tool = self.tool_name.clone().unwrap_or_else(|| "tool".into());
                let detail = self.tool_input.as_ref().and_then(|v| {
                    v.get("command")
                        .or_else(|| v.get("file_path"))
                        .and_then(|c| c.as_str())
                        .map(str::to_owned)
                });
                HookEvent::PreTool { tool, detail }
            }
            "Stop" | "AfterAgent" => HookEvent::TurnEnd,
            _ => HookEvent::Ignore,
        }
    }
}

/// Snippet to merge into the agent's hook config. `unwreck hooks --write` installs
/// the same block; this print path is for reviewing it first.
pub fn install_snippet(agent: Agent) -> String {
    let cmd = format!("unwreck hook {}", agent.name());
    let (start, pre, end, pre_matcher, note) = match agent {
        Agent::Claude => (
            "UserPromptSubmit",
            "PreToolUse",
            "Stop",
            "Bash|Write|Edit|MultiEdit|NotebookEdit",
            "~/.claude/settings.json or .claude/settings.local.json",
        ),
        Agent::Codex => (
            "UserPromptSubmit",
            "PreToolUse",
            "Stop",
            "*",
            "~/.codex/hooks.json",
        ),
        Agent::Gemini => (
            "BeforeAgent",
            "BeforeTool",
            "AfterAgent",
            "run_shell_command|write_file|replace",
            "~/.gemini/settings.json",
        ),
    };
    let h = |m: Option<&str>| {
        let hook = serde_json::json!({"type": "command", "command": cmd});
        match m {
            Some(m) => serde_json::json!({"matcher": m, "hooks": [hook]}),
            None => serde_json::json!({"hooks": [hook]}),
        }
    };
    let v = serde_json::json!({"hooks": {
        start: [h(None)],
        pre: [h(Some(pre_matcher))],
        end: [h(None)],
    }});
    format!(
        "// add to {note}\n{}",
        serde_json::to_string_pretty(&v).expect("static json")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_pre_tool_bash() {
        let p = parse(r#"{"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Bash","tool_input":{"command":"rm -rf src/gen"}}"#).unwrap();
        assert_eq!(
            p.event(),
            HookEvent::PreTool {
                tool: "Bash".into(),
                detail: Some("rm -rf src/gen".into())
            }
        );
    }

    #[test]
    fn codex_turn_id_and_gemini_names() {
        let p =
            parse(r#"{"hook_event_name":"UserPromptSubmit","turn_id":"t1","prompt":"fix tests"}"#)
                .unwrap();
        assert_eq!(p.turn_key().as_deref(), Some("t1"));
        let g = parse(r#"{"hook_event_name":"AfterAgent"}"#).unwrap();
        assert_eq!(g.event(), HookEvent::TurnEnd);
    }

    #[test]
    fn docs_fixtures_parse() {
        // From vendor docs, not captured (see fixtures/README.md).
        let claude_start = parse(include_str!(
            "../fixtures/docs/claude-UserPromptSubmit.json"
        ))
        .unwrap();
        assert!(matches!(claude_start.event(), HookEvent::TurnStart { .. }));
        let claude_pre =
            parse(include_str!("../fixtures/docs/claude-PreToolUse-Bash.json")).unwrap();
        assert!(matches!(claude_pre.event(), HookEvent::PreTool { .. }));
        let claude_stop = parse(include_str!("../fixtures/docs/claude-Stop.json")).unwrap();
        assert_eq!(claude_stop.event(), HookEvent::TurnEnd);
        let codex = parse(include_str!("../fixtures/docs/codex-UserPromptSubmit.json")).unwrap();
        assert_eq!(codex.turn_key().as_deref(), Some("turn-docs-1"));
        let gemini_pre = parse(include_str!("../fixtures/docs/gemini-BeforeTool.json")).unwrap();
        assert!(matches!(gemini_pre.event(), HookEvent::PreTool { .. }));
        let gemini_end = parse(include_str!("../fixtures/docs/gemini-AfterAgent.json")).unwrap();
        assert_eq!(gemini_end.event(), HookEvent::TurnEnd);
        let _ = parse(include_str!(
            "../fixtures/docs/codex-PreToolUse-apply_patch.json"
        ))
        .unwrap();
        let bash = parse(include_str!("../fixtures/docs/codex-PreToolUse-Bash.json")).unwrap();
        assert_eq!(
            parse(include_str!("../fixtures/docs/codex-Stop.json"))
                .unwrap()
                .event(),
            HookEvent::TurnEnd
        );
        assert_eq!(
            bash.event(),
            HookEvent::PreTool {
                tool: "Bash".into(),
                detail: Some("rm -rf src/gen .env".into()),
            }
        );
        let _ = parse(include_str!("../fixtures/docs/gemini-BeforeAgent.json")).unwrap();
        assert!(
            install_snippet(Agent::Codex).contains("\"*\""),
            "Codex PreToolUse matcher must be *"
        );
    }

    #[test]
    fn malformed_payload_is_error() {
        assert!(parse("{").is_err());
        assert!(parse("").is_err());
        assert!(parse("[]").is_err());
    }

    #[test]
    fn codex_shell_tool_aliases_are_pre_tool() {
        for name in ["Bash", "shell", "exec_command", "exec"] {
            let p = parse(&format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"{name}","tool_input":{{"command":"rm -rf src/gen"}}}}"#
            ))
            .unwrap();
            assert!(
                matches!(p.event(), HookEvent::PreTool { tool, .. } if tool == name),
                "{name}"
            );
        }
    }
}
