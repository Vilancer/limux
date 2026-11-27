use serde_json::{json, Value};

use super::{provider, AgentState, HookReading, Notice};
use crate::agent_hooks::AgentKind;

fn read(agent: AgentKind, event: &str, payload: Value) -> HookReading {
    provider(agent).read(event, &payload)
}

fn state(agent: AgentKind, event: &str, payload: Value) -> Option<&'static str> {
    read(agent, event, payload).state.map(AgentState::wire_name)
}

#[test]
fn claude_hook_events_map_to_live_states() {
    use AgentKind::Claude;
    let table = [
        (
            "session-start",
            json!({"hook_event_name": "SessionStart", "source": "startup"}),
            Some("idle"),
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix it"}),
            Some("working"),
        ),
        (
            "post-tool-use",
            json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"}),
            Some("working"),
        ),
        (
            "permission-request",
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash",
                   "tool_input": {"command": "rm -rf build"}}),
            Some("needs_approval"),
        ),
        (
            "stop",
            json!({"hook_event_name": "Stop", "stop_hook_active": false}),
            Some("idle"),
        ),
        (
            "session-end",
            json!({"hook_event_name": "SessionEnd", "reason": "exit"}),
            Some("ended"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "permission_prompt",
                   "message": "Claude needs your permission to use Bash"}),
            Some("needs_approval"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "elicitation_dialog",
                   "message": "Claude has a question"}),
            Some("needs_input"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "idle_prompt",
                   "message": "Claude is waiting for your input"}),
            Some("idle"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "auth_success"}),
            None,
        ),
        // Claude Code 2.1.3xx: background agents and workers.
        (
            "notification",
            json!({"hook_event_name": "Notification",
                   "notification_type": "worker_permission_prompt",
                   "message": "builder needs permission for Bash"}),
            Some("needs_approval"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "agent_needs_input",
                   "message": "ca842437 needs your input"}),
            Some("needs_input"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification",
                   "notification_type": "elicitation_url_dialog"}),
            Some("needs_input"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "agent_completed",
                   "message": "ca842437 finished"}),
            None,
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "push_notification",
                   "message": "Build is green"}),
            None,
        ),
        // Older Claude Code: no type, only the message.
        (
            "notification",
            json!({"message": "Claude needs your permission to use Write"}),
            Some("needs_approval"),
        ),
        (
            "notification",
            json!({"message": "Claude is waiting for your input"}),
            Some("idle"),
        ),
        ("notification", json!({"message": "something else"}), None),
        ("subagent-stop", json!({}), None),
    ];
    for (event, payload, expected) in table {
        let label = format!("{event} {payload}");
        assert_eq!(state(Claude, event, payload), expected, "{label}");
    }
}

#[test]
fn claude_asks_at_once_and_leaves_the_notice_to_its_notification() {
    let reading = read(
        AgentKind::Claude,
        "permission-request",
        json!({"tool_name": "Bash", "tool_input": {"command": "cargo publish"}}),
    );
    assert_eq!(reading.state, Some(AgentState::NeedsApproval));
    assert_eq!(reading.detail.as_deref(), Some("Bash: cargo publish"));
    // The permission_prompt Notification about six seconds later posts it,
    // and adds no line of its own: the tool above stays.
    assert_eq!(reading.notice, None);
    let note = read(
        AgentKind::Claude,
        "notification",
        json!({"notification_type": "permission_prompt", "message": "Claude needs your permission"}),
    );
    assert_eq!(note.state, Some(AgentState::NeedsApproval));
    assert_eq!(note.detail, None);
    assert!(note.notice.is_some());
    // A question asks through the same dialog (measured on Claude Code
    // 2.1.330): it waits for an answer, not a yes or no.
    let reading = read(
        AgentKind::Claude,
        "permission-request",
        json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
               "tool_input": {"questions": [{"question": "Which colour do you prefer, red or blue?",
                                             "header": "Colour", "options": []}]}}),
    );
    assert_eq!(reading.state, Some(AgentState::NeedsInput));
    assert_eq!(
        reading.detail.as_deref(),
        Some("Which colour do you prefer, red or blue?")
    );
}

#[test]
fn claude_notifies_once_the_main_agent_is_done_with_what_it_said() {
    use AgentKind::Claude;
    let running = json!({
        "last_assistant_message": "started",
        "background_tasks": [
            {"id": "b1", "type": "shell", "status": "running", "description": "dev server"},
            {"id": "a1", "type": "subagent", "status": "running", "description": "Reply with pong"},
        ],
    });
    // Subagents still run: no notification, and it keeps working.
    let reading = read(Claude, "stop", running);
    assert_eq!(reading.notice, None);
    assert_eq!(reading.state, Some(AgentState::Working));
    assert_eq!(
        reading.detail.as_deref(),
        Some("Waiting on: Reply with pong")
    );
    // Only a background shell command left (it may never end): done.
    let done = json!({
        "last_assistant_message": "\nThe **sidebar** fix is in `window.rs`.\nMore detail.",
        "background_tasks": [
            {"id": "b1", "type": "shell", "status": "running", "description": "dev server"},
        ],
    });
    let reading = read(Claude, "stop", done);
    assert_eq!(reading.state, Some(AgentState::Idle));
    assert_eq!(
        reading.notice.map(|notice| notice.title_body("Claude")),
        Some((
            "Claude finished".to_string(),
            "The sidebar fix is in window.rs.".to_string()
        ))
    );
    assert_eq!(
        read(Claude, "stop", json!({}))
            .notice
            .map(|notice| notice.title_body("Claude")),
        Some(("Claude finished".to_string(), "task complete".to_string()))
    );
    // A background agent finishing does not notify; one asking does.
    assert_eq!(
        read(
            Claude,
            "notification",
            json!({"notification_type": "agent_completed", "message": "ca842437 finished"})
        )
        .notice,
        None
    );
    assert_eq!(
        read(
            Claude,
            "notification",
            json!({"notification_type": "agent_needs_input", "message": "builder needs your input"})
        )
        .notice,
        Some(Notice::NeedsYou("builder needs your input".to_string()))
    );
    assert_eq!(
        read(
            Claude,
            "notification",
            json!({"notification_type": "push_notification", "message": "Build is green"})
        )
        .notice
        .map(|notice| notice.title_body("Claude")),
        Some(("Claude".to_string(), "Build is green".to_string()))
    );
}

#[test]
fn claude_idle_reminder_sets_idle_without_a_second_notice() {
    use AgentKind::Claude;
    // About a minute after a turn its Stop announced as finished, Claude
    // Code reminds that it waits. It asks for nothing new.
    for payload in [
        json!({"hook_event_name": "Notification", "notification_type": "idle_prompt",
               "message": "Claude is waiting for your input"}),
        // Older Claude Code: no type, only the message.
        json!({"message": "Claude is waiting for your input"}),
    ] {
        let reading = read(Claude, "notification", payload.clone());
        assert_eq!(reading.state, Some(AgentState::Idle), "{payload}");
        assert_eq!(reading.notice, None, "{payload}");
    }
}

#[test]
fn claude_subagent_tools_leave_the_main_agent_state_alone() {
    use AgentKind::Claude;
    let subagent = |event: &str| {
        read(
            Claude,
            event,
            json!({"hook_event_name": "PostToolUse", "agent_id": "a1b2",
                   "agent_type": "general-purpose", "tool_name": "Grep",
                   "tool_input": {"pattern": "not_text|NUL|control"}}),
        )
    };
    // It reports as the subagent's, with no line: Limux keeps "Waiting on"
    // and a question of the main agent, and clears only a subagent's wait.
    for event in ["post-tool-use", "PostToolUse", "PreToolUse"] {
        let reading = subagent(event);
        assert_eq!(reading.state, Some(AgentState::Working), "{event}");
        assert!(reading.subagent, "{event}");
        assert_eq!(reading.detail, None, "{event}");
    }
    // The main agent's own tools still report.
    let main = read(
        Claude,
        "post-tool-use",
        json!({"tool_name": "Grep", "agent_id": "", "tool_input": {"pattern": "fn main"}}),
    );
    assert_eq!(main.state, Some(AgentState::Working));
    assert!(!main.subagent);
    assert_eq!(main.detail.as_deref(), Some("Grep: fn main"));
    // A subagent that needs permission still needs the user.
    let ask = read(
        Claude,
        "permission-request",
        json!({"agent_id": "a1b2", "tool_name": "Bash", "tool_input": {"command": "rm -rf build"}}),
    );
    assert_eq!(ask.state, Some(AgentState::NeedsApproval));
    assert!(ask.subagent);
    // So does a background agent or worker that asks through a notification.
    for kind in ["agent_needs_input", "worker_permission_prompt"] {
        let note = read(
            Claude,
            "notification",
            json!({"notification_type": kind, "message": "builder needs you"}),
        );
        assert!(note.subagent, "{kind}");
    }
    let note = read(
        Claude,
        "notification",
        json!({"notification_type": "elicitation_dialog", "message": "Claude has a question"}),
    );
    assert!(!note.subagent);
}

#[test]
fn the_detail_says_what_the_agent_does() {
    use AgentKind::Claude;
    let table = [
        (
            "prompt-submit",
            json!({"prompt": "\n  fix the sidebar tabs\nand more"}),
            Some("fix the sidebar tabs"),
        ),
        (
            "post-tool-use",
            json!({"tool_name": "Bash", "tool_input": {"command": "cargo test -p limux\n"}}),
            Some("Bash: cargo test -p limux"),
        ),
        (
            "post-tool-use",
            json!({"tool_name": "Edit", "tool_input": {"file_path": "/a/b/window.rs"}}),
            Some("Edit: window.rs"),
        ),
        (
            "post-tool-use",
            json!({"tool_name": "Grep", "tool_input": {"pattern": "fn main", "path": "/a/src"}}),
            Some("Grep: fn main"),
        ),
        (
            "post-tool-use",
            json!({"tool_name": "TodoWrite", "tool_input": {"todos": []}}),
            Some("TodoWrite"),
        ),
        (
            "notification",
            json!({"message": "Claude needs your permission to use Bash"}),
            Some("Claude needs your permission to use Bash"),
        ),
        ("stop", json!({"message": "done"}), None),
        ("post-tool-use", json!({}), None),
    ];
    for (event, payload, detail) in table {
        let label = format!("{event} {payload}");
        assert_eq!(
            read(Claude, event, payload).detail.as_deref(),
            detail,
            "{label}"
        );
    }
    let long = "x".repeat(200);
    let detail = read(Claude, "prompt-submit", json!({ "prompt": long }))
        .detail
        .unwrap();
    assert_eq!(detail.chars().count(), 80);
    assert!(detail.ends_with('…'));
}

#[test]
fn codex_hook_events_map_to_live_states() {
    use AgentKind::Codex;
    // Codex 0.171's hook input schemas.
    let base = |event: &str| {
        json!({"hook_event_name": event, "session_id": "s", "cwd": "/w", "model": "m",
               "turn_id": "t", "transcript_path": null, "permission_mode": "default"})
    };
    let with = |event: &str, extra: Value| {
        let mut payload = base(event);
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };
    let table = [
        ("session-start", base("SessionStart"), Some("idle"), None),
        (
            "prompt-submit",
            with("UserPromptSubmit", json!({"prompt": "add a test"})),
            Some("working"),
            Some("add a test"),
        ),
        (
            "post-tool-use",
            with(
                "PostToolUse",
                json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"},
                       "tool_response": "ok", "tool_use_id": "c1"}),
            ),
            Some("working"),
            Some("Bash: cargo test"),
        ),
        (
            "post-tool-use",
            with(
                "PostToolUse",
                json!({"tool_name": "apply_patch",
                       "tool_input": {"command": "*** Begin Patch\n*** Update File: src/main.rs"}}),
            ),
            Some("working"),
            Some("Edit: main.rs"),
        ),
        (
            "permission-request",
            with(
                "PermissionRequest",
                json!({"tool_name": "Bash", "tool_input": {"command": "git push"}}),
            ),
            Some("needs_approval"),
            Some("Bash: git push"),
        ),
        (
            "stop",
            with(
                "Stop",
                json!({"last_assistant_message": "Done.", "stop_hook_active": false}),
            ),
            Some("idle"),
            None,
        ),
        ("interrupt", base("Interrupt"), Some("idle"), None),
        (
            "session-end",
            with("SessionEnd", json!({"reason": "exit"})),
            Some("ended"),
            None,
        ),
        (
            "pre-compact",
            with("PreCompact", json!({"trigger": "auto"})),
            None,
            None,
        ),
    ];
    for (event, payload, expected, detail) in table {
        let label = format!("{event} {payload}");
        let reading = read(Codex, event, payload);
        assert_eq!(
            reading.state.map(AgentState::wire_name),
            expected,
            "{label}"
        );
        assert_eq!(reading.detail.as_deref(), detail, "{label}");
    }
}

#[test]
fn codex_asks_and_finishes_with_notices_and_answers_its_schema() {
    use AgentKind::Codex;
    assert_eq!(
        read(
            Codex,
            "permission-request",
            json!({"tool_name": "Bash", "tool_input": {"command": "git push"}})
        )
        .notice
        .map(|notice| notice.title_body("Codex")),
        Some((
            "Codex needs you".to_string(),
            "approve Bash: git push".to_string()
        ))
    );
    assert_eq!(
        read(
            Codex,
            "stop",
            json!({"last_assistant_message": "Added the test.\nIt passes."})
        )
        .notice,
        Some(Notice::Finished("Added the test.".to_string()))
    );
    // The user interrupted: nothing to tell them.
    assert_eq!(read(Codex, "interrupt", json!({})).notice, None);
    // `Interrupt` takes only `systemMessage`: no `continue` in the answer.
    let codex = provider(Codex);
    assert_eq!(codex.output("interrupt", &json!({})), Some(json!({})));
    assert_eq!(codex.output("stop", &json!({})), None);
}

#[test]
fn gemini_hook_events_map_to_live_states() {
    use AgentKind::Gemini;
    let table = [
        (
            "session-start",
            json!({"hook_event_name": "SessionStart", "source": "startup"}),
            Some("idle"),
            None,
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "BeforeAgent", "prompt": "explain main.rs"}),
            Some("working"),
            Some("explain main.rs"),
        ),
        (
            "post-tool-use",
            json!({"hook_event_name": "AfterTool", "tool_name": "run_shell_command",
                   "tool_input": {"command": "ls -la"}, "tool_response": {}}),
            Some("working"),
            Some("Shell: ls -la"),
        ),
        (
            "post-tool-use",
            json!({"hook_event_name": "AfterTool", "tool_name": "replace",
                   "tool_input": {"file_path": "/w/src/lib.rs"}}),
            Some("working"),
            Some("Edit: lib.rs"),
        ),
        (
            "notification",
            json!({"hook_event_name": "Notification", "notification_type": "ToolPermission",
                   "message": "Allow run_shell_command: rm -rf dist?", "details": {}}),
            Some("needs_approval"),
            Some("Allow run_shell_command: rm -rf dist?"),
        ),
        (
            "stop",
            json!({"hook_event_name": "AfterAgent", "prompt": "explain",
                   "prompt_response": "It starts the app.", "stop_hook_active": false}),
            Some("idle"),
            None,
        ),
        (
            "session-end",
            json!({"hook_event_name": "SessionEnd", "reason": "exit"}),
            Some("ended"),
            None,
        ),
    ];
    for (event, payload, expected, detail) in table {
        let label = format!("{event} {payload}");
        let reading = read(Gemini, event, payload);
        assert_eq!(
            reading.state.map(AgentState::wire_name),
            expected,
            "{label}"
        );
        assert_eq!(reading.detail.as_deref(), detail, "{label}");
    }
    assert_eq!(
        read(
            Gemini,
            "stop",
            json!({"prompt_response": "It starts the app.\nThen it waits."})
        )
        .notice,
        Some(Notice::Finished("It starts the app.".to_string()))
    );
}

#[test]
fn opencode_plugin_events_map_to_live_states() {
    use AgentKind::OpenCode;
    let table = [
        (
            "session-start",
            json!({"hook_event_name": "session.created"}),
            Some("idle"),
            None,
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "chat.message", "prompt": "rename the module"}),
            Some("working"),
            Some("rename the module"),
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "session.status", "status": "busy"}),
            Some("working"),
            None,
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "session.status", "status": "idle"}),
            None,
            None,
        ),
        (
            "tool",
            json!({"hook_event_name": "tool.execute.before", "tool_name": "bash",
                   "tool_input": {"command": "bun test", "description": "Run tests"}}),
            Some("working"),
            Some("Bash: bun test"),
        ),
        (
            "tool",
            json!({"hook_event_name": "tool.execute.before", "tool_name": "edit",
                   "tool_input": {"filePath": "/w/src/index.ts"}}),
            Some("working"),
            Some("Edit: index.ts"),
        ),
        (
            "permission",
            // OpenCode 1.19.6, measured: a pipeline asks with one pattern
            // per command, which the plugin joins.
            json!({"hook_event_name": "permission.asked", "permission": "bash",
                   "pattern": "ls -1 | wc -l"}),
            Some("needs_approval"),
            Some("Bash: ls -1 | wc -l"),
        ),
        (
            "question",
            json!({"hook_event_name": "question.asked", "question": "Which database?"}),
            Some("needs_input"),
            Some("Which database?"),
        ),
        (
            "answered",
            json!({"hook_event_name": "permission.replied"}),
            Some("working"),
            None,
        ),
        (
            "stop",
            json!({"hook_event_name": "session.idle"}),
            Some("idle"),
            None,
        ),
        (
            "cleanup",
            json!({"hook_event_name": "session.deleted"}),
            Some("ended"),
            None,
        ),
        (
            "prompt-submit",
            json!({"hook_event_name": "session.updated"}),
            None,
            None,
        ),
    ];
    for (event, payload, expected, detail) in table {
        let label = format!("{event} {payload}");
        let reading = read(OpenCode, event, payload);
        assert_eq!(
            reading.state.map(AgentState::wire_name),
            expected,
            "{label}"
        );
        assert_eq!(reading.detail.as_deref(), detail, "{label}");
    }
    assert_eq!(
        read(
            OpenCode,
            "question",
            json!({"hook_event_name": "question.asked", "question": "Which database?"})
        )
        .notice
        .map(|notice| notice.title_body("OpenCode")),
        Some((
            "OpenCode needs you".to_string(),
            "Which database?".to_string()
        ))
    );
    assert_eq!(
        read(OpenCode, "stop", json!({"hook_event_name": "session.idle"})).notice,
        Some(Notice::Finished("task complete".to_string()))
    );
    // The plugin sends what the assistant last wrote; the title is the
    // fallback.
    assert_eq!(
        read(
            OpenCode,
            "stop",
            json!({"hook_event_name": "session.idle", "title": "Count entries",
                   "last_message": "There are **4** entries.\nDone."})
        )
        .notice,
        Some(Notice::Finished("There are 4 entries.".to_string()))
    );
}

#[test]
fn every_json_hook_agent_installs_the_events_it_reads() {
    for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini] {
        let events = provider(agent).hook_events();
        assert!(!events.is_empty(), "{agent:?}");
        for (native, limux) in events {
            // Session restore needs these; the rest feed the live state.
            let reading = provider(agent).read(limux, &json!({"hook_event_name": native}));
            if *limux != "notification" {
                assert!(
                    reading.state.is_some(),
                    "{agent:?} installs {native} but reads no state from it"
                );
            }
        }
    }
}

#[test]
fn the_hook_templates_install_what_each_provider_reads() {
    for (agent, template) in [
        (
            AgentKind::Claude,
            include_str!("../../../../hooks/claude-settings.json"),
        ),
        (
            AgentKind::Codex,
            include_str!("../../../../hooks/codex-hooks.json"),
        ),
        (
            AgentKind::Gemini,
            include_str!("../../../../hooks/gemini-settings.json"),
        ),
    ] {
        let template: Value = serde_json::from_str(template).unwrap();
        let hooks = template["hooks"].as_object().unwrap();
        let mut installed: Vec<&str> = hooks.keys().map(String::as_str).collect();
        let mut expected: Vec<&str> = provider(agent)
            .hook_events()
            .iter()
            .map(|(native, _)| *native)
            .collect();
        installed.sort_unstable();
        expected.sort_unstable();
        assert_eq!(installed, expected, "{agent:?} template");
        for (native, limux) in provider(agent).hook_events() {
            let command = hooks[*native][0]["hooks"][0]["command"].as_str().unwrap();
            assert!(
                command.contains(&format!("hooks {} {limux} ", agent.store_name())),
                "{agent:?} {native}: {command}"
            );
        }
    }
}

#[test]
fn the_restore_wrapper_cleanup_ends_every_agent_session() {
    // A restored agent that exits before its own hooks run (Claude Code
    // refused an untrusted folder) must not stay listed as quiet.
    for agent in [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::OpenCode,
        AgentKind::Agy,
    ] {
        let payload = json!({"session_id": "s", "hook_event_name": "Cleanup"});
        let reading = super::read(agent, "cleanup", &payload);
        assert_eq!(
            reading.state.map(AgentState::wire_name),
            Some("ended"),
            "{agent:?}"
        );
        assert_eq!(reading.notice, None, "{agent:?}");
    }
}
