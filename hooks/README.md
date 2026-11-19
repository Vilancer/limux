# Limux Agent Hooks

These templates wire supported coding-agent hook systems into Limux session
restore tracking. They are intentionally limited to Codex, Claude Code, and
Gemini CLI until the OpenCode hook path is ready.

The preferred install path is the CLI installer:

```bash
limux hooks setup
```

That writes the equivalent configuration into each agent's user config:

| Agent | Destination |
|---|---|
| Codex | `$CODEX_HOME/hooks.json` or `~/.codex/hooks.json` |
| Claude Code | `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json` |
| Gemini CLI | `~/.gemini/settings.json` |

Use the files in this directory as canonical examples when reviewing or
manually repairing an agent config:

- `codex-hooks.json`
- `claude-settings.json`
- `gemini-settings.json`

Each command calls `limux --json hooks <agent> <event>` and is guarded by a
per-agent disable variable:

```bash
LIMUX_CODEX_HOOKS_DISABLED=1
LIMUX_CLAUDE_HOOKS_DISABLED=1
LIMUX_GEMINI_HOOKS_DISABLED=1
```

## Live agent state (Claude Code)

Claude Code hooks also report what the agent is doing to the terminal that runs
it, through the `agent.activity` control method, so the sidebar can show
working and waiting terminals:

| Claude Code event | State |
|---|---|
| `SessionStart`, `Stop` | idle (waiting for a prompt) |
| `UserPromptSubmit`, `PostToolUse` | working |
| `Notification` with `permission_prompt` | needs approval |
| `Notification` with `elicitation_dialog` | needs input |
| `Notification` with `idle_prompt` | idle |
| `SessionEnd` | ended |

Each report also carries one line of what the agent is doing, shown under its
terminal in the sidebar: the first line of the prompt, the tool it just ran
(`Bash: cargo test`, `Edit: window.rs`), or the notification message. It stays
on your machine; the hook only sends it to the local Limux socket.

`PostToolUse` is new: it tells Limux that a tool ran after you approved it.
Hooks installed by an older Limux lack it, so a request you approved keeps
showing as waiting until Claude's turn ends. Run `limux hooks setup claude`
again to add it, once per Claude config directory if you use several
(`CLAUDE_CONFIG_DIR=<dir> limux hooks setup claude`), and restart running
Claude Code sessions. The report is best effort: outside a
Limux terminal, over SSH or with `LIMUX_NOTIFY_OSC=1` nothing is sent, and the
agent is never held up. Codex, Gemini and OpenCode keep their notifications and
report no live state yet.
