//! Claude Code: `claude` hooks in `settings.json`.

use serde_json::Value;

use super::{
    last_said, payload_tool_detail, AgentProvider, AgentState, HookReading, Notice, TITLE_CHARS,
};
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
            "Stop" | "stop" => stop(payload),
            "finished" => HookReading::notice(Notice::Finished(said(payload))),
            "SessionStart" | "session-start" => HookReading::state(AgentState::Idle),
            "UserPromptSubmit" | "prompt-submit" => HookReading::state(AgentState::Working)
                .with_detail(hook_str(payload, &["prompt"]).map(str::to_owned)),
            // A subagent's tool runs under the main session's id. It says
            // nothing of the main agent, which may wait for it ("Waiting
            // on") or ask the user something: Limux lets it answer only a
            // subagent's own wait, and it adds no line.
            "PreToolUse" | "PostToolUse" | "post-tool-use" if from_subagent(payload) => {
                HookReading::state(AgentState::Working).by_subagent(true)
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
            }
            .by_subagent(from_subagent(payload)),
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
    let kind = hook_str(payload, &["notification_type", "notificationType"]);
    let subagent = from_subagent(payload)
        || matches!(kind, Some("worker_permission_prompt" | "agent_needs_input"));
    let state = match kind {
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
    .by_subagent(subagent)
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

/// The end of a turn. Claude Code lists the work still in flight
/// (`background_tasks`) and the wake-ups it has scheduled (`session_crons`):
/// it wakes up on its own when any of it ends or fires, so the turn's end is
/// not always the end of the work.
fn stop(payload: &Value) -> HookReading {
    let (agents, commands) = in_flight(payload);
    // Background agents report back soon: it keeps working.
    if !agents.is_empty() {
        let detail = match agents.as_slice() {
            [one] => format!("Waiting on: {one}"),
            many => format!("Waiting on {} background agents", many.len()),
        };
        return HookReading::state(AgentState::Working).with_detail(Some(detail));
    }
    // A command or a monitor may run for hours (a server never ends), so it
    // is quiet, but says what it waits on and is not "finished".
    if !commands.is_empty() {
        let on = match commands.as_slice() {
            [one] => crate::one_line(one, TITLE_CHARS),
            many => format!("{} background commands", many.len()),
        };
        return HookReading::state(AgentState::Idle)
            .in_background(true)
            .with_detail(Some(format!("Background: {on}")))
            .with_notice(Notice::WaitingOn {
                on,
                said: said(payload),
            });
    }
    let done = HookReading::state(AgentState::Idle).with_notice(Notice::Finished(said(payload)));
    match wake_up(payload) {
        Some(when) => done.in_background(true).with_detail(Some(when)),
        None => done.in_background(false),
    }
}

/// The first line of what Claude last said, for a notification.
fn said(payload: &Value) -> String {
    last_said(payload, &["last_assistant_message"], &["message", "reason"])
}

/// The session's work still in flight (running or pending): background
/// agents and the like, then commands and monitors (shells, which may never
/// end). Each named by its description, else its command.
fn in_flight(payload: &Value) -> (Vec<&str>, Vec<&str>) {
    let mut agents = Vec::new();
    let mut commands = Vec::new();
    let tasks = payload.get("background_tasks").and_then(Value::as_array);
    for task in tasks.into_iter().flatten() {
        if !matches!(hook_str(task, &["status"]), Some("running" | "pending")) {
            continue;
        }
        match hook_str(task, &["type"]) {
            Some("shell" | "monitor") => commands.push(
                hook_str(task, &["description", "command"]).unwrap_or("a background command"),
            ),
            _ => agents.push(hook_str(task, &["description"]).unwrap_or("background agent")),
        }
    }
    (agents, commands)
}

/// When a scheduled wake-up (ScheduleWakeup, CronCreate, /loop) brings the
/// session back: "Wakes at 14:30" for a one-shot whose cron names a time of
/// day, else "Wakes on a schedule".
fn wake_up(payload: &Value) -> Option<String> {
    let crons = payload.get("session_crons").and_then(Value::as_array)?;
    if crons.is_empty() {
        return None;
    }
    // Only a one-shot names a time; a recurring one (or one that does not
    // say) fires again and again.
    let at = crons
        .iter()
        .filter(|cron| cron.get("recurring").and_then(Value::as_bool) == Some(false))
        .find_map(|cron| hook_str(cron, &["schedule"]).and_then(time_of_day));
    Some(at.map_or_else(
        || "Wakes on a schedule".to_owned(),
        |(hour, minute)| format!("Wakes at {hour:02}:{minute:02}"),
    ))
}

/// The time of day of a cron expression whose minute and hour are plain
/// numbers ("30 14 27 11 *").
fn time_of_day(schedule: &str) -> Option<(u8, u8)> {
    let mut fields = schedule.split_whitespace();
    let minute: u8 = fields.next()?.parse().ok()?;
    let hour: u8 = fields.next()?.parse().ok()?;
    (minute < 60 && hour < 24).then_some((hour, minute))
}
