# Limux Agent Hooks

These templates wire supported coding-agent hook systems into Limux session
restore tracking and the live agent state in the sidebar. Default setup covers
Codex, Claude Code, Gemini CLI and Antigravity (`agy`); OpenCode remains opt-in
via `limux hooks setup opencode`, which writes its plugin.

The preferred install path is the CLI installer:

```bash
limux hooks setup
```

That writes the equivalent configuration into each agent's user config:

| Agent | Destination |
|---|---|
| Codex | `$CODEX_HOME/hooks.json` or `~/.codex/hooks.json` |
| Claude Code | `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json` |
| Gemini CLI | `$GEMINI_CLI_HOME/.gemini/settings.json` or `~/.gemini/settings.json` |
| Antigravity (`agy`) | `~/.gemini/config/hooks.json` |

Use the files in this directory as canonical examples when reviewing or
manually repairing an agent config:

- `codex-hooks.json`
- `claude-settings.json`
- `gemini-settings.json`
- `agy-hooks.json`

Each command calls `limux --json hooks <agent> <event>` and is guarded by a
per-agent disable variable:

```bash
LIMUX_CODEX_HOOKS_DISABLED=1
LIMUX_CLAUDE_HOOKS_DISABLED=1
LIMUX_GEMINI_HOOKS_DISABLED=1
LIMUX_AGY_HOOKS_DISABLED=1
```

## Live agent state

The hooks also report what the agent is doing to the terminal that runs it,
through the `agent.activity` control method, so the sidebar can show working
and waiting terminals. Each agent's hooks are read by its provider in
`rust/limux-cli/src/agent_providers/`, which turns them into the same states:

| State | Claude Code | Codex | Gemini CLI | OpenCode (plugin) |
|---|---|---|---|---|
| working | `UserPromptSubmit`, `PostToolUse` | `UserPromptSubmit`, `PostToolUse` | `BeforeAgent`, `AfterTool` | `chat.message`, `session.status` busy, `tool.execute.before`, a reply to a permission or question |
| needs approval | `PermissionRequest`, `Notification` `permission_prompt` | `PermissionRequest` | `Notification` `ToolPermission` | `permission.asked` |
| needs input | `PermissionRequest` for `AskUserQuestion`, `Notification` `elicitation_dialog` | | | `question.asked` |
| idle | `SessionStart`, `Stop`, `Notification` `idle_prompt` | `SessionStart`, `Stop`, `Interrupt` | `SessionStart`, `AfterAgent` | `session.created`, `session.idle` |
| ended | `SessionEnd` | `SessionEnd` | `SessionEnd` | `session.deleted` |

Each report also carries one line of what the agent is doing, shown under its
terminal in the sidebar: the first line of the prompt, the tool it just ran
(`Bash: cargo test`, `Edit: window.rs`), the question, or what it asks to run.
It stays on your machine; the hook only sends it to the local Limux socket.
When a turn ends, the notification says what the agent last said.

Claude Code's `PermissionRequest` fires as the dialog opens; its
`permission_prompt` Notification comes about six seconds later, or never when
you answer sooner. A question (AskUserQuestion) uses the same dialog, so it is
told apart by its tool. Codex has no Notification hook: it notifies on
`PermissionRequest` and `Stop`. OpenCode's plugin skips the events of a
subagent's child session, whose end is not the end of your session. Likewise,
a Claude Code subagent's tool hooks (they carry an `agent_id`) are sent with
`"subagent": true`: they leave the terminal's state and line alone ("Waiting
on" a background agent, or a question of the main agent) and only clear a wait
that a subagent raised, once it goes on. The `idle_prompt` reminder Claude
Code sends about a minute after a turn ends marks it idle without a second
notification.

Hooks installed by an older Limux lack the newer events, so approvals show
late and a request you approved keeps showing as waiting until the turn ends.
Run `limux hooks setup <agent>` again to add them (for Claude, once per config
directory if you use several: `CLAUDE_CONFIG_DIR=<dir> limux hooks setup
claude`) and restart running sessions. Codex then asks you to trust the new
hooks once (`/hooks`, `t`).

The report is best effort: outside a Limux terminal nothing is sent, over SSH
or with `LIMUX_NOTIFY_OSC=1` only the notification goes back through the
terminal, and the agent is never held up. Another agent can be added with a
provider of its own: the hooks it installs, and how one hook call reads.

## Snapshotting already-running agent sessions

Hooks record session IDs as agents start or run turns. If you already have live
agent tabs open before installing hooks, or want to flush restorable state
right before restarting or upgrading Limux, snapshot running processes and flush
`session.json`:

```bash
# Preview which running Limux-hosted agents can be snapshotted
limux hooks snapshot --dry-run

# Record live running agent sessions into ~/.local/state/limux/*-hook-sessions.json
# and flush ~/.local/share/limux/session.json from the running host
limux hooks snapshot --save-session

# Or flush session.json directly
limux save-session
```
