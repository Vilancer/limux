//! Agents without a live-state mapping (Antigravity): notifications only,
//! read from the event name, as before providers existed.

use serde_json::Value;

use super::{finished, AgentProvider, HookReading, Notice};
use crate::hook_str;

pub(super) struct Generic;

impl AgentProvider for Generic {
    fn read(&self, event: &str, payload: &Value) -> HookReading {
        match event {
            "Notification" | "notification" => HookReading::notice(Notice::NeedsYou(
                hook_str(payload, &["message", "notification"])
                    .unwrap_or("waiting for input")
                    .to_owned(),
            )),
            "Stop" | "stop" | "finished" if stop_is_quiet(payload) => HookReading::default(),
            "Stop" | "stop" | "finished" => HookReading::notice(stop_notice(payload)),
            "SessionStart" | "session-start" | "SessionEnd" | "session-end"
            | "UserPromptSubmit" | "prompt-submit" | "PreInvocation" | "pre-invocation"
            | "PostInvocation" | "post-invocation" | "PreToolUse" | "PostToolUse"
            | "post-tool-use" | "SubagentStop" | "subagent-stop" | "Cleanup" | "cleanup"
            | "restore-exit" => HookReading::default(),
            other => HookReading::notice(Notice::Event {
                event: other.to_owned(),
                message: hook_str(payload, &["message", "summary"])
                    .unwrap_or_default()
                    .to_owned(),
            }),
        }
    }
}

/// Antigravity reports a stop that is not the end of the work (more steps
/// to come, or the user cancelled).
fn stop_is_quiet(payload: &Value) -> bool {
    payload.get("fullyIdle").and_then(Value::as_bool) == Some(false)
        || hook_str(payload, &["terminationReason"]) == Some("USER_CANCELED")
}

/// Why Antigravity stopped: an error, a limit it hit, or what it last said.
fn stop_notice(payload: &Value) -> Notice {
    if let Some(error) = hook_str(payload, &["error"]) {
        return Notice::Failed(error.to_owned());
    }
    let limit = match hook_str(payload, &["terminationReason"]) {
        Some("ERROR") => return Notice::Failed("stopped with an error".to_owned()),
        Some("MAX_INVOCATIONS") => "hit maximum invocations",
        Some("MAX_FORCED_INVOCATIONS") => "hit maximum forced invocations",
        Some("MAX_TOKEN_BUDGET_EXCEEDED") => "exceeded token budget",
        Some("TERMINAL_CUSTOM_HOOK") => "stopped by custom hook",
        _ => return finished(payload, &["last_assistant_message"], &["message", "reason"]),
    };
    Notice::Finished(limit.to_owned())
}
