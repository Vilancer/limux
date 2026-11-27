//! Claude Code: `claude` hooks in `settings.json`.

use serde_json::Value;

use super::{finished, payload_tool_detail, AgentProvider, AgentState, HookReading, Notice};
use crate::hook_str;

pub(super) struct Claude;

impl AgentProvider for Claude {
    fn hook_events(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "prompt-submit"),
            ("Stop", "stop"),
            ("Notification", "notification"),
            // Resumes the working state after the user approved a tool.
            ("PostToolUse", "post-tool-use"),
            // Fires as the permission dialog opens; its Notification comes
            // about six seconds later, or never when answered sooner.
            ("PermissionRequest", "permission-request"),
            ("SessionEnd", "session-end"),
        ]
    }

    fn read(&self, event: &str, payload: &Value) -> HookReading {
        match event {
            "Notification" | "notification" => notification(payload),
            "Stop" | "stop" => match running_subagents(payload).as_slice() {
                // It wakes up on its own when they finish: not done yet.
                [] => HookReading::state(AgentState::Idle).with_notice(finished(
                    payload,
                    &["last_assistant_message"],
                    &["message", "reason"],
                )),
                [one] => HookReading::state(AgentState::Working)
                    .with_detail(Some(format!("Waiting on: {one}"))),
                many => HookReading::state(AgentState::Working)
                    .with_detail(Some(format!("Waiting on {} background agents", many.len()))),
            },
            "finished" => HookReading::notice(finished(
                payload,
                &["last_assistant_message"],
                &["message", "reason"],
            )),
            "SessionStart" | "session-start" => HookReading::state(AgentState::Idle),
            "UserPromptSubmit" | "prompt-submit" => HookReading::state(AgentState::Working)
                .with_detail(hook_str(payload, &["prompt"]).map(str::to_owned)),
            // A subagent's tool runs under the main session's id. It says
            // nothing of the main agent: that one may be idle while the
            // subagent works ("Waiting on" stays), or waiting on the user.
            "PreToolUse" | "PostToolUse" | "post-tool-use" if from_subagent(payload) => {
                HookReading::default()
            }
            "PreToolUse" | "PostToolUse" | "post-tool-use" => {
                HookReading::state(AgentState::Working)
                    .with_detail(payload_tool_detail(payload, |tool| tool))
            }
            // The Notification that follows posts the notice. A question
            // (AskUserQuestion) asks through the same dialog.
            "PermissionRequest" | "permission-request" => match hook_str(payload, &["tool_name"]) {
                Some("AskUserQuestion") => {
                    HookReading::state(AgentState::NeedsInput).with_detail(first_question(payload))
                }
                _ => HookReading::state(AgentState::NeedsApproval)
                    .with_detail(payload_tool_detail(payload, |tool| tool)),
            },
            "SessionEnd" | "session-end" => HookReading::state(AgentState::Ended),
            "SubagentStop" | "subagent-stop" | "Cleanup" | "cleanup" | "restore-exit" => {
                HookReading::default()
            }
            other => HookReading::notice(Notice::Event {
                event: other.to_owned(),
                message: hook_str(payload, &["message", "summary"])
                    .unwrap_or_default()
                    .to_owned(),
            }),
        }
    }
}

/// Claude Code sends one Notification hook for several reasons. Newer
/// versions name it in `notification_type`; older ones only in the message.
/// A background agent or worker of the session asking for permission or
/// input needs the user in this terminal too.
fn notification(payload: &Value) -> HookReading {
    let message = hook_str(payload, &["message", "notification"]);
    let needs_you = || Notice::NeedsYou(message.unwrap_or("waiting for input").to_owned());
    let state = match hook_str(payload, &["notification_type", "notificationType"]) {
        Some("permission_prompt" | "worker_permission_prompt") => Some(AgentState::NeedsApproval),
        Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
            Some(AgentState::NeedsInput)
        }
        // A reminder about a minute after the turn ended, which its Stop
        // already announced ("finished"): it asks for nothing new.
        Some("idle_prompt") => return HookReading::state(AgentState::Idle),
        // A background agent finishing is not the session finishing: the
        // main agent picks its result up and reports when it is done.
        Some("agent_completed") => return HookReading::default(),
        // A message Claude chose to send does not ask for anything.
        Some("push_notification") => {
            return HookReading::notice(Notice::Note(message.unwrap_or("update").to_owned()))
        }
        Some(_) => None,
        None => {
            let lower = message.unwrap_or_default().to_ascii_lowercase();
            if lower.contains("permission") {
                Some(AgentState::NeedsApproval)
            } else if lower.contains("waiting for your input") {
                return HookReading::state(AgentState::Idle);
            } else {
                None
            }
        }
    };
    // Newer Claude Code says only "Claude needs your permission"; its
    // PermissionRequest already named the tool or the question, so a line
    // that says nothing more is left out and the first one stays. Older
    // versions name the tool here ("... permission to use Bash").
    let detail = hook_str(payload, &["message"])
        .filter(|message| !matches!(*message, "Claude needs your permission"))
        .map(str::to_owned);
    HookReading {
        state,
        ..HookReading::default()
    }
    .with_detail(detail)
    .with_notice(needs_you())
}

/// Claude Code names the subagent a hook ran in; the main agent's hooks
/// carry no `agent_id`.
fn from_subagent(payload: &Value) -> bool {
    hook_str(payload, &["agent_id"]).is_some()
}

/// The first question an AskUserQuestion call asks.
fn first_question(payload: &Value) -> Option<String> {
    let question = payload
        .get("tool_input")?
        .get("questions")?
        .as_array()?
        .first()?;
    hook_str(question, &["question", "header"]).map(str::to_owned)
}

/// Subagents of a Claude Code session that still run when its turn ends. It
/// wakes up on its own when they finish, so the turn's end is not the end
/// of the work. Background shell commands do not count: a server never ends.
pub(super) fn running_subagents(payload: &Value) -> Vec<&str> {
    payload
        .get("background_tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|task| {
            hook_str(task, &["status"]) == Some("running")
                && hook_str(task, &["type"]).is_some_and(|kind| kind != "shell")
        })
        .map(|task| hook_str(task, &["description"]).unwrap_or("background agent"))
        .collect()
}
