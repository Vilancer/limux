//! OpenCode: a plugin (written by `limux hooks setup opencode`) that runs
//! `limux hooks opencode <event>` for the bus events below, with the
//! OpenCode event type in `hook_event_name` and the fields Limux needs
//! (`status`, `tool_name`, `tool_input`, `permission`, `question`, `prompt`).
//! Events of a subagent's child session are not sent: its end is not the
//! session's end.

use serde_json::Value;

use super::{tool_detail, AgentProvider, AgentState, HookReading, Notice};
use crate::hook_str;

pub(super) struct OpenCode;

impl AgentProvider for OpenCode {
    fn read(&self, event: &str, payload: &Value) -> HookReading {
        let kind = hook_str(payload, &["hook_event_name", "event"]).unwrap_or(event);
        match kind {
            "session.created" => HookReading::state(AgentState::Idle),
            "chat.message" => HookReading::state(AgentState::Working)
                .with_detail(hook_str(payload, &["prompt"]).map(str::to_owned)),
            "session.status" => match hook_str(payload, &["status"]) {
                Some("busy") => HookReading::state(AgentState::Working),
                Some("retry") => HookReading::state(AgentState::Working).with_detail(
                    hook_str(payload, &["message"]).map(|message| format!("Retrying: {message}")),
                ),
                // `session.idle` follows and posts the notice.
                _ => HookReading::default(),
            },
            "tool.execute.before" => {
                let detail = hook_str(payload, &["tool_name"])
                    .map(|tool| tool_detail(&capitalized(tool), payload.get("tool_input")));
                HookReading::state(AgentState::Working).with_detail(detail)
            }
            "permission.asked" | "permission.updated" => {
                let what = hook_str(payload, &["permission"]).map(|permission| {
                    match hook_str(payload, &["pattern"]) {
                        Some(pattern) => format!("{}: {pattern}", capitalized(permission)),
                        None => capitalized(permission),
                    }
                });
                HookReading::state(AgentState::NeedsApproval)
                    .with_detail(what.clone())
                    .with_notice(super::approval_notice(what))
            }
            "question.asked" => {
                let question = hook_str(payload, &["question"]).map(str::to_owned);
                HookReading::state(AgentState::NeedsInput)
                    .with_detail(question.clone())
                    .with_notice(Notice::NeedsYou(
                        question.unwrap_or_else(|| "has a question".to_owned()),
                    ))
            }
            "permission.replied" | "question.replied" | "question.rejected" => {
                HookReading::state(AgentState::Working)
            }
            // What it last wrote, else the session's title.
            "session.idle" => HookReading::state(AgentState::Idle).with_notice(super::finished(
                payload,
                &["last_message"],
                &["title"],
            )),
            "session.error" => HookReading::state(AgentState::Idle).with_notice(Notice::Note(
                hook_str(payload, &["message"])
                    .unwrap_or("stopped with an error")
                    .to_owned(),
            )),
            "session.deleted" => HookReading::state(AgentState::Ended),
            _ => HookReading::default(),
        }
    }
}

/// OpenCode names tools and permissions in lower case ("bash", "edit").
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
