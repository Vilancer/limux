//! Gemini CLI: hooks in `~/.gemini/settings.json`. Its payloads follow
//! Claude Code's shape (`tool_name`, `tool_input`); a permission prompt
//! comes as a Notification of type `ToolPermission`.

use serde_json::Value;

use super::{finished, payload_tool_detail, AgentProvider, AgentState, HookReading, Notice};
use crate::hook_str;

pub(super) struct Gemini;

impl AgentProvider for Gemini {
    fn hook_events(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("SessionStart", "session-start"),
            ("BeforeAgent", "prompt-submit"),
            ("AfterTool", "post-tool-use"),
            ("Notification", "notification"),
            ("AfterAgent", "stop"),
            ("SessionEnd", "session-end"),
        ]
    }

    fn read(&self, event: &str, payload: &Value) -> HookReading {
        match event {
            "SessionStart" | "session-start" => HookReading::state(AgentState::Idle),
            "BeforeAgent" | "prompt-submit" => HookReading::state(AgentState::Working)
                .with_detail(hook_str(payload, &["prompt"]).map(str::to_owned)),
            "BeforeTool" | "AfterTool" | "post-tool-use" => HookReading::state(AgentState::Working)
                .with_detail(payload_tool_detail(payload, tool_name)),
            "Notification" | "notification" => {
                let message = hook_str(payload, &["message"]).map(str::to_owned);
                let state = (hook_str(payload, &["notification_type"]) == Some("ToolPermission"))
                    .then_some(AgentState::NeedsApproval);
                HookReading {
                    state,
                    ..HookReading::default()
                }
                .with_detail(message.clone())
                .with_notice(Notice::NeedsYou(
                    message.unwrap_or_else(|| "waiting for approval".to_owned()),
                ))
            }
            "AfterAgent" | "stop" => HookReading::state(AgentState::Idle).with_notice(finished(
                payload,
                &["prompt_response"],
                &["message", "reason"],
            )),
            "SessionEnd" | "session-end" => HookReading::state(AgentState::Ended),
            _ => HookReading::default(),
        }
    }
}

/// Gemini CLI's tools under the names its own interface shows.
fn tool_name(tool: &str) -> &str {
    match tool {
        "run_shell_command" => "Shell",
        "read_file" => "ReadFile",
        "read_many_files" => "ReadManyFiles",
        "write_file" => "WriteFile",
        "replace" => "Edit",
        "glob" => "FindFiles",
        "search_file_content" | "grep" => "SearchText",
        "list_directory" => "ReadFolder",
        "web_fetch" => "WebFetch",
        "google_web_search" => "GoogleSearch",
        "save_memory" => "SaveMemory",
        other => other,
    }
}
