//! Agent providers: how each coding agent's hooks read in Limux's terms.
//!
//! Every agent has its own hook system, event names and payloads. A provider
//! turns one hook call into a [`HookReading`]: the agent's live state in its
//! terminal (sent as `agent.activity`), one line of what it is doing, and the
//! notice to post, if any. Everything after that (the socket, the sidebar,
//! notifications, session restore) is shared, so supporting another agent
//! means one provider and the list of hooks it installs.

mod claude;
mod codex;
mod gemini;
mod generic;
mod opencode;
#[cfg(test)]
mod tests;

use serde_json::Value;

use crate::agent_hooks::AgentKind;
use crate::{hook_str, one_line, plain_text};

/// What the agent in a terminal is doing, as the sidebar shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// On a prompt: thinking or running tools.
    Working,
    /// Asks to run something and waits for a yes or no.
    NeedsApproval,
    /// Asks a question and waits for an answer.
    NeedsInput,
    /// Waits for the next prompt.
    Idle,
    /// The session is over.
    Ended,
}

impl AgentState {
    /// The `state` of the `agent.activity` control method.
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsApproval => "needs_approval",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
            Self::Ended => "ended",
        }
    }
}

/// A notification to post, worded the same for every agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// "Codex needs you": it waits for the user.
    NeedsYou(String),
    /// "Codex finished", with what it last said.
    Finished(String),
    /// "Antigravity failed", with the error.
    Failed(String),
    /// "Codex", with a message the agent chose to send.
    Note(String),
    /// "Codex: <event>", for a hook event Limux does not know.
    Event { event: String, message: String },
}

impl Notice {
    pub(crate) fn title_body(self, label: &str) -> (String, String) {
        match self {
            Self::NeedsYou(body) => (format!("{label} needs you"), body),
            Self::Finished(body) => (format!("{label} finished"), body),
            Self::Failed(body) => (format!("{label} failed"), body),
            Self::Note(body) => (label.to_string(), body),
            Self::Event { event, message } => (format!("{label}: {event}"), message),
        }
    }
}

/// What one hook call says, in Limux's terms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HookReading {
    /// The new live state; `None` leaves the terminal's state as it is.
    pub(crate) state: Option<AgentState>,
    /// One line of what the agent is doing; `None` keeps the previous one.
    pub(crate) detail: Option<String>,
    pub(crate) notice: Option<Notice>,
}

impl HookReading {
    fn state(state: AgentState) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }

    fn notice(notice: Notice) -> Self {
        Self {
            notice: Some(notice),
            ..Self::default()
        }
    }

    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.map(|text| one_line(&text, DETAIL_CHARS));
        self
    }

    fn with_notice(mut self, notice: Notice) -> Self {
        self.notice = Some(notice);
        self
    }
}

/// One coding agent's hooks.
pub(crate) trait AgentProvider: Sync {
    /// The hooks `limux hooks setup` writes into the agent's JSON hook
    /// config, as (the agent's event, the event Limux is called with).
    /// Empty for agents installed another way (a plugin, their own file).
    fn hook_events(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Read one hook call. `event` is the event Limux was called with (see
    /// [`AgentProvider::hook_events`]); `payload` is what the agent sent.
    fn read(&self, event: &str, payload: &Value) -> HookReading;

    /// What the hook answers on stdout, when the agent expects something
    /// other than Limux's usual answer for the event.
    fn output(&self, _event: &str, _payload: &Value) -> Option<Value> {
        None
    }
}

pub(crate) fn provider(agent: AgentKind) -> &'static dyn AgentProvider {
    match agent {
        AgentKind::Claude => &claude::Claude,
        AgentKind::Codex => &codex::Codex,
        AgentKind::Gemini => &gemini::Gemini,
        AgentKind::OpenCode => &opencode::OpenCode,
        AgentKind::Agy => &generic::Generic,
    }
}

/// How long the detail line may be.
const DETAIL_CHARS: usize = 80;
/// How long a notification's text may be.
const NOTICE_CHARS: usize = 140;

/// The notice for a finished turn: the first line of what the agent last
/// said (it tells the tasks apart), else `fallback` keys, else "task complete".
fn finished(payload: &Value, message_keys: &[&str], fallback_keys: &[&str]) -> Notice {
    let body = hook_str(payload, message_keys)
        .map(|message| one_line(&plain_text(message), NOTICE_CHARS))
        .filter(|line| !line.is_empty())
        .or_else(|| hook_str(payload, fallback_keys).map(str::to_owned))
        .unwrap_or_else(|| "task complete".to_owned());
    Notice::Finished(body)
}

/// The notice for an approval the agent waits for, naming what it asks to run.
fn approval_notice(what: Option<String>) -> Notice {
    Notice::NeedsYou(
        what.map(|what| format!("approve {what}"))
            .unwrap_or_else(|| "waiting for approval".to_owned()),
    )
}

/// "Bash: cargo test", "Edit: window.rs": a tool and what it works on, from
/// whichever field the tool's input has. Fields are tried in order, so a
/// search shows its pattern rather than the folder it searches, and a file
/// shows only its name.
fn tool_detail(tool: &str, input: Option<&Value>) -> String {
    let field = |keys: &[&str]| input.and_then(|input| hook_str(input, keys));
    let command = input.and_then(|input| match input.get("command") {
        Some(Value::String(command)) => Some(command.trim().to_owned()),
        // Codex passes a command as its arguments.
        Some(Value::Array(argv)) => Some(
            argv.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    });
    let target = command
        .filter(|command| !command.is_empty())
        .or_else(|| field(&["pattern", "query", "url", "description"]).map(str::to_owned))
        .or_else(|| {
            field(&[
                "file_path",
                "filePath",
                "notebook_path",
                "absolute_path",
                "path",
            ])
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_owned())
            })
        });
    match target {
        Some(target) => format!("{tool}: {target}"),
        None => tool.to_owned(),
    }
}

/// The tool detail of a payload with Claude Code's shape (`tool_name`,
/// `tool_input`), which Codex and Gemini CLI share.
fn payload_tool_detail(payload: &Value, rename: impl Fn(&str) -> &str) -> Option<String> {
    let tool = hook_str(payload, &["tool_name"])?;
    Some(tool_detail(rename(tool), payload.get("tool_input")))
}
