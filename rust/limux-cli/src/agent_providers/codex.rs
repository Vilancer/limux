//! Codex: hooks in `$CODEX_HOME/hooks.json`, with Claude Code's payload
//! shape. Codex has no Notification hook: a permission request is the only
//! time it waits for the user, and `PermissionRequest` fires as it asks.

use serde_json::{json, Value};

use super::{finished, payload_tool_detail, AgentProvider, AgentState, HookReading};
use crate::hook_str;

pub(super) struct Codex;

impl AgentProvider for Codex {
    fn hook_events(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "prompt-submit"),
            ("PostToolUse", "post-tool-use"),
            ("PermissionRequest", "permission-request"),
            ("Stop", "stop"),
            ("Interrupt", "interrupt"),
            ("SessionEnd", "session-end"),
        ]
    }

    fn read(&self, event: &str, payload: &Value) -> HookReading {
        match event {
            "SessionStart" | "session-start" => HookReading::state(AgentState::Idle),
            "UserPromptSubmit" | "prompt-submit" => HookReading::state(AgentState::Working)
                .with_detail(hook_str(payload, &["prompt"]).map(str::to_owned)),
            "PreToolUse" | "PostToolUse" | "post-tool-use" => {
                HookReading::state(AgentState::Working).with_detail(detail(payload))
            }
            "PermissionRequest" | "permission-request" => {
                let what = detail(payload);
                HookReading::state(AgentState::NeedsApproval)
                    .with_detail(what.clone())
                    .with_notice(super::approval_notice(what))
            }
            "Stop" | "stop" => HookReading::state(AgentState::Idle).with_notice(finished(
                payload,
                &["last_assistant_message"],
                &["message", "reason"],
            )),
            // The user stopped it: no notice, they are right there.
            "Interrupt" | "interrupt" => HookReading::state(AgentState::Idle),
            "SessionEnd" | "session-end" => HookReading::state(AgentState::Ended),
            _ => HookReading::default(),
        }
    }

    /// Codex checks each answer against the event's schema; `Interrupt` and
    /// `SessionEnd` take no `continue`.
    fn output(&self, event: &str, _payload: &Value) -> Option<Value> {
        matches!(
            event,
            "Interrupt" | "interrupt" | "SessionEnd" | "session-end"
        )
        .then(|| json!({}))
    }
}

/// What a tool works on. A patch names the first file it touches.
fn detail(payload: &Value) -> Option<String> {
    let patched = payload
        .get("tool_input")
        .and_then(|input| hook_str(input, &["command", "input", "patch"]))
        .filter(|_| hook_str(payload, &["tool_name"]) == Some("apply_patch"))
        .and_then(|patch| {
            patch.lines().find_map(|line| {
                ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                    .iter()
                    .find_map(|marker| line.strip_prefix(marker))
            })
        });
    match patched {
        Some(path) => Some(super::tool_detail(
            "Edit",
            Some(&serde_json::json!({ "file_path": path.trim() })),
        )),
        None => payload_tool_detail(payload, tool_name),
    }
}

/// Codex's tools under the names people know.
fn tool_name(tool: &str) -> &str {
    match tool {
        "shell" | "exec_command" | "local_shell" => "Bash",
        "apply_patch" => "Edit",
        other => other,
    }
}
