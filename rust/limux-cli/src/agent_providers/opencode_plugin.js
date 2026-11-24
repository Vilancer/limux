// limux-opencode-session-plugin v3
// Installed by `limux hooks opencode install`. Do not edit manually.
//
// Sends OpenCode's session events to `limux hooks opencode <event>`: session
// restore, and the live state shown in Limux's sidebar (working, waiting for
// a permission or an answer, finished). Events of a subagent's child session
// are not sent: its end is not the end of the user's session.

import { spawnSync } from "node:child_process";
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const LIMUX_COMMAND = __LIMUX_COMMAND__;

function debug(outcome, details = {}) {
  if (process.env.LIMUX_OPENCODE_HOOK_DEBUG !== "1" && outcome !== "spawn_failed") return;
  try {
    const dir = process.env.LIMUX_AGENT_HOOK_STATE_DIR || (process.env.XDG_STATE_HOME ? join(process.env.XDG_STATE_HOME, "limux") : join(process.env.HOME || ".", ".local/state/limux"));
    mkdirSync(dir, { recursive: true });
    appendFileSync(join(dir, "opencode-plugin-debug.jsonl"), JSON.stringify({
      time: Date.now() / 1000,
      outcome,
      details
    }) + "\n");
  } catch (_) {}
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === "string" && value.trim().length > 0) return value.trim();
  }
  return null;
}

function props(event) {
  return (event && typeof event === "object" && event.properties) || {};
}

function data(event) {
  return (event && typeof event === "object" && event.data) || {};
}

function info(event) {
  const p = props(event);
  const d = data(event);
  return (p.info && typeof p.info === "object" && p.info) || (d.info && typeof d.info === "object" && d.info) || {};
}

function eventType(event) {
  const raw = firstString(event && event.type, event && event.name);
  if (!raw) return null;
  if (raw === "sync") return firstString(event && event.name);
  return raw.endsWith(".1") ? raw.slice(0, -2) : raw;
}

function sessionId(event) {
  const p = props(event);
  const d = data(event);
  const i = info(event);
  return firstString(p.sessionID, p.sessionId, p.session_id, d.sessionID, d.sessionId, d.session_id, i.id, event && event.sessionID, event && event.sessionId);
}

function cwd(ctx, event) {
  const p = props(event);
  const d = data(event);
  const i = info(event);
  return firstString(p.cwd, p.directory, d.cwd, d.directory, i.directory, i.path, ctx && ctx.directory, process.cwd());
}

function launchExecutable() {
  return firstString(process.env.LIMUX_OPENCODE_EXECUTABLE, "opencode");
}

// Sessions started by the task tool, each session's title, which messages
// are the assistant's, and the last text it wrote per session.
const childSessions = new Set();
const titles = new Map();
const assistantMessages = new Set();
const lastText = new Map();

// What Limux reads from each event, besides its type.
function details(type, event) {
  const p = props(event);
  if (type === "session.status") {
    const status = p.status || {};
    return { status: firstString(status.type), message: firstString(status.message) };
  }
  if (type === "permission.asked" || type === "permission.updated") {
    const patterns = Array.isArray(p.patterns) ? p.patterns : [];
    return {
      permission: firstString(p.permission, p.type),
      // A pipeline asks once with one pattern per command.
      pattern: firstString(patterns.join(" | "), typeof p.pattern === "string" ? p.pattern : null, Array.isArray(p.pattern) ? p.pattern.join(" | ") : null, p.title)
    };
  }
  if (type === "question.asked") {
    const questions = Array.isArray(p.questions) ? p.questions : [];
    const first = questions[0] || {};
    return { question: firstString(first.question, first.header, first.text) };
  }
  if (type === "session.error") {
    const error = p.error || {};
    return { message: firstString(error.data && error.data.message, error.message, error.name) };
  }
  return {};
}

function send(kind, ctx, event, extra = {}) {
  if (process.env.LIMUX_OPENCODE_HOOKS_DISABLED === "1") {
    debug("skip_disabled", { kind });
    return;
  }
  if (!process.env.LIMUX_SURFACE_ID) {
    debug("skip_missing_surface", { kind, type: eventType(event), hasWorkspace: !!process.env.LIMUX_WORKSPACE_ID });
    return;
  }
  const sid = sessionId(event);
  if (!sid) {
    debug("skip_missing_session", { kind, type: eventType(event), keys: Object.keys(event || {}) });
    return;
  }
  if (childSessions.has(sid)) {
    debug("skip_child_session", { kind, type: eventType(event) });
    return;
  }
  const type = eventType(event);
  const payload = {
    session_id: sid,
    cwd: cwd(ctx, event),
    hook_event_name: type,
    event: type,
    title: titles.get(sid) || null,
    ...details(type, event),
    ...extra
  };
  try {
    const command = process.env.LIMUX_BIN || LIMUX_COMMAND;
    const result = spawnSync(command, ["hooks", "opencode", kind], {
      input: JSON.stringify(payload),
      encoding: "utf8",
      stdio: ["pipe", "ignore", "ignore"],
      timeout: 5000,
      env: {
        ...process.env,
        LIMUX_AGENT_LAUNCH_ARGV: launchExecutable(),
        LIMUX_AGENT_LAUNCH_EXECUTABLE: launchExecutable(),
        LIMUX_AGENT_LAUNCH_CWD: cwd(ctx, event)
      }
    });
    debug("spawned", { kind, type, status: result.status, error: result.error && String(result.error), command });
  } catch (error) {
    debug("spawn_failed", { kind, type, error: String(error) });
  }
}

// A plugin hook call, sent like a bus event of its own type.
function hookEvent(type, sessionID) {
  return { type, properties: { sessionID } };
}

const limuxSessionRestore = async (ctx) => {
  debug("plugin_started", { directory: ctx && ctx.directory, hasSurface: !!process.env.LIMUX_SURFACE_ID, hasWorkspace: !!process.env.LIMUX_WORKSPACE_ID });
  return {
    event: async ({ event }) => {
      const type = eventType(event);
      debug("event", { type, rawType: event && event.type, rawName: event && event.name });
      if (!type) return;
      const i = info(event);
      if (type === "message.updated" && i.id && i.role === "assistant") assistantMessages.add(i.id);
      if (type === "message.part.updated") {
        const part = props(event).part || {};
        if (part.type === "text" && !part.synthetic && assistantMessages.has(part.messageID) && typeof part.text === "string") {
          lastText.set(part.sessionID, part.text);
        }
        return;
      }
      if ((type === "session.created" || type === "session.updated") && i.id) {
        if (i.parentID) childSessions.add(i.id);
        if (typeof i.title === "string") titles.set(i.id, i.title);
      }
      if (type === "session.created") send("session-start", ctx, event);
      if (type === "session.updated" || type === "session.status" || type === "session.compacted") send("prompt-submit", ctx, event);
      if (type === "permission.asked" || type === "permission.updated") send("permission-request", ctx, event);
      if (type === "question.asked") send("notification", ctx, event);
      if (type === "permission.replied" || type === "question.replied" || type === "question.rejected") send("post-tool-use", ctx, event);
      if (type === "session.idle") {
        const sid = sessionId(event);
        send("stop", ctx, event, { last_message: firstString(lastText.get(sid)) });
        lastText.delete(sid);
      }
      if (type === "session.error") send("session-end", ctx, event);
      if (type === "session.deleted") send("cleanup", ctx, event);
    },
    "chat.message": async (input, output) => {
      const parts = (output && Array.isArray(output.parts)) ? output.parts : [];
      const prompt = parts.filter((part) => part && part.type === "text" && !part.synthetic).map((part) => part.text).join("\n");
      send("prompt-submit", ctx, hookEvent("chat.message", input && input.sessionID), { prompt: firstString(prompt) });
    },
    "tool.execute.before": async (input, output) => {
      send("post-tool-use", ctx, hookEvent("tool.execute.before", input && input.sessionID), {
        tool_name: input && input.tool,
        tool_input: (output && output.args) || {}
      });
    }
  };
};

export const LimuxSessionRestore = limuxSessionRestore;
export default limuxSessionRestore;
