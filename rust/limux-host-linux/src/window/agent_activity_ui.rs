//! Live agent state in the window: the `agent.activity` control method, the
//! one signal each project row and top-bar pill shows, the expandable
//! session rows under a project, and the unread dwell.
//!
//! The rules live in `crate::agent_activity`; this module only reads panes,
//! applies events to `AppState::agent_activity` and paints widgets.

use super::*;
use crate::agent_activity::{
    self, ActivityCounts, AgentEvent, AgentPhase, ProjectSignal, GLYPH_UNREAD, GLYPH_WAITING,
    GLYPH_WORKING,
};

/// How long a project must stay on screen before its unread marks clear, so
/// passing through projects (`Ctrl+Alt+PageDown`) does not wipe them.
const UNREAD_DWELL: std::time::Duration = std::time::Duration::from_millis(1000);
/// How often closed terminals are pruned and open session lists refreshed.
const ACTIVITY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

const GLYPH_EXITED: &str = "\u{2715}"; // ✕

/// One row of a project's session list, as last drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SessionRowView {
    pane_id: u32,
    tab_id: String,
    title: String,
    /// First terminal of a pane after the first one: a hairline goes above.
    pane_start: bool,
    /// Not the pane's active tab, so not on screen when the project is.
    /// Only marked in the full list (`▸`).
    background: bool,
    /// The pane's active tab, marked with an edge in the full list.
    shown: bool,
    glyph: &'static str,
    state_label: &'static str,
    agent: Option<String>,
    /// What it does or asks, or its last message, in one line.
    detail: Option<String>,
    /// Working, waiting or unread: shown even while the list is closed.
    active: bool,
}

// ---------------------------------------------------------------------------
// Counts and signals
// ---------------------------------------------------------------------------

/// Working and waiting terminals of a workspace.
pub(super) fn workspace_activity_counts(s: &AppState, workspace_id: &str) -> ActivityCounts {
    let mut counts = ActivityCounts::default();
    if s.agent_activity.is_empty() {
        return counts;
    }
    for tab_id in pane::terminal_tab_ids_in_workspace(workspace_id) {
        counts.add_phase(s.agent_activity.phase(&tab_id));
    }
    counts
}

/// Paint the row dot and the pill dot from the project signal.
pub(super) fn set_workspace_signal_visual(
    workspace: &Workspace,
    signal: ProjectSignal,
    counts: ActivityCounts,
) {
    let dot = &workspace.notify_dot;
    for class in [
        "limux-notify-dot",
        "limux-notify-dot-hidden",
        "limux-notify-dot-waiting",
        "limux-notify-dot-working",
    ] {
        dot.remove_css_class(class);
    }
    let (glyph, classes, tooltip): (&str, &[&str], Option<String>) = match signal {
        ProjectSignal::Waiting => (
            GLYPH_WAITING,
            &["limux-notify-dot", "limux-notify-dot-waiting"],
            agent_activity::counts_summary(counts),
        ),
        ProjectSignal::Unread => (
            GLYPH_UNREAD,
            &["limux-notify-dot"],
            Some("Unread".to_string()),
        ),
        ProjectSignal::Working => (
            GLYPH_WORKING,
            &["limux-notify-dot-working"],
            agent_activity::counts_summary(counts),
        ),
        ProjectSignal::None => (GLYPH_UNREAD, &["limux-notify-dot-hidden"], None),
    };
    dot.set_label(glyph);
    for class in classes {
        dot.add_css_class(class);
    }
    dot.set_tooltip_text(tooltip.as_deref());
    let label = match &tooltip {
        Some(tooltip) => format!("{}, {tooltip}", workspace.name),
        None => workspace.name.clone(),
    };
    workspace
        .sidebar_row
        .update_property(&[gtk::accessible::Property::Label(&label)]);

    // The pill only says "needs attention": waiting, else unread.
    let pill_dot = &workspace.indicator_unread_dot;
    pill_dot.remove_css_class("limux-indicator-waiting-dot");
    match signal {
        ProjectSignal::Waiting => {
            pill_dot.set_label(GLYPH_WAITING);
            pill_dot.add_css_class("limux-indicator-waiting-dot");
        }
        _ => pill_dot.set_label(GLYPH_UNREAD),
    }
    workspace.indicator_button.set_tooltip_text(
        signal
            .needs_attention()
            .then(|| match signal {
                ProjectSignal::Waiting => format!("{}: needs you", workspace.name),
                _ => format!("{}: unread", workspace.name),
            })
            .as_deref(),
    );
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub(super) fn handle_agent_activity(
    state: &State,
    target: &WorkspaceTarget,
    surface_hint: &str,
    event: AgentEvent,
) -> Result<serde_json::Value, BridgeError> {
    let preferred = workspace_index_for_target(&state.borrow(), target)
        .or_else(|| (!state.borrow().workspaces.is_empty()).then_some(0))
        .ok_or_else(|| BridgeError::not_found("workspace not found"))?;
    let pane::HintResolution::Unique((_, _, tab_id)) =
        surface_tab_target(state, preferred, false, surface_hint, true)
    else {
        return Err(BridgeError::not_found("terminal not found"));
    };
    // A held, exited terminal has no agent; a late hook must not revive it.
    if pane::terminal_tabs_by_id().get(&tab_id).copied() == Some(true) {
        return Ok(serde_json::json!({ "tab_id": tab_id, "changed": false }));
    }
    let changed = state.borrow_mut().agent_activity.apply(AgentEvent {
        tab_id: tab_id.clone(),
        ..event
    });
    if changed {
        refresh_activity(state);
    }
    Ok(serde_json::json!({ "tab_id": tab_id, "changed": changed }))
}

/// Forget terminals that closed or exited. Returns whether any were dropped.
fn prune_activity(state: &State) -> bool {
    if state.borrow().agent_activity.is_empty() {
        return false;
    }
    let tabs = pane::terminal_tabs_by_id();
    state
        .borrow_mut()
        .agent_activity
        .retain_tabs(|tab_id| tabs.get(tab_id) == Some(&false))
}

/// Settle agents that went quiet without a hook (interrupted, denied or
/// killed). Returns whether any changed.
fn settle_quiet_activity(state: &State) -> bool {
    if state.borrow().agent_activity.is_empty() {
        return false;
    }
    let now = crate::agent_activity::now_ms();
    note_seen_tabs(state, now);
    let mut s = state.borrow_mut();
    let AppState {
        agent_activity,
        tab_seen_ms,
        ..
    } = &mut *s;
    agent_activity.settle_quiet(
        now,
        |tab_id| {
            pane::terminal_silence(tab_id)
                .map(|silent| u64::try_from(silent.as_millis()).unwrap_or(u64::MAX))
        },
        |tab_id, at_ms| tab_seen_ms.get(tab_id).is_some_and(|seen| *seen > at_ms),
    )
}

/// Remember that the tabs on screen (the active tab of each shown pane of the
/// shown project, in an active window) were seen now. Closed tabs are dropped.
fn note_seen_tabs(state: &State, now: u64) {
    let root = {
        let s = state.borrow();
        if !s.window.is_active() {
            None
        } else {
            s.active_workspace().map(|workspace| workspace.root.clone())
        }
    };
    let mut shown = Vec::new();
    if let Some(root) = root {
        let mut panes = Vec::new();
        collect_leaf_panes(&root, &mut panes);
        shown.extend(
            panes
                .iter()
                .filter_map(pane::active_surface_summary)
                .map(|surface| surface.tab_id),
        );
    }
    let mut s = state.borrow_mut();
    let AppState {
        agent_activity,
        tab_seen_ms,
        ..
    } = &mut *s;
    tab_seen_ms.retain(|tab_id, _| agent_activity.get(tab_id).is_some());
    for tab_id in shown {
        tab_seen_ms.insert(tab_id, now);
    }
}

/// Repaint everything that reads agent state: project and pill signals, group
/// headers and their waiting rows, and open session lists.
pub(super) fn refresh_activity(state: &State) {
    prune_activity(state);
    let ids: Vec<String> = state
        .borrow()
        .workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .collect();
    for id in &ids {
        paint_workspace_unread(state, id);
    }
    sync_tab_signs(state);
    sidebar_groups::sync_row_visibility(state);
}

/// The same sign in each terminal's tab: waiting or working.
fn sync_tab_signs(state: &State) {
    let s = state.borrow();
    pane::sync_tab_agent_signs(|tab_id| match s.agent_activity.phase(tab_id)? {
        AgentPhase::NeedsApproval => Some(pane::TabAgentSign {
            glyph: GLYPH_WAITING,
            css_class: "limux-session-waiting",
            tooltip: "Needs your approval",
        }),
        AgentPhase::NeedsInput => Some(pane::TabAgentSign {
            glyph: GLYPH_WAITING,
            css_class: "limux-session-waiting",
            tooltip: "Has a question for you",
        }),
        AgentPhase::Working => Some(pane::TabAgentSign {
            glyph: GLYPH_WORKING,
            css_class: "limux-session-working",
            tooltip: "Agent working",
        }),
        AgentPhase::Idle => None,
    });
}

pub(super) fn install_activity_timer(state: &State) {
    let weak = Rc::downgrade(state);
    glib::timeout_add_local(ACTIVITY_REFRESH_INTERVAL, move || {
        let Some(state) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let settled = settle_quiet_activity(&state);
        if prune_activity(&state) || settled {
            refresh_activity(&state);
        } else {
            // Titles change and tabs open and close: keep every session list
            // current (each rebuilds only when its rows changed).
            let ids: Vec<String> = state
                .borrow()
                .workspaces
                .iter()
                .map(|workspace| workspace.id.clone())
                .collect();
            for id in ids {
                refresh_session_rows(&state, &id, false);
            }
        }
        glib::ControlFlow::Continue
    });
}

// ---------------------------------------------------------------------------
// Unread dwell
// ---------------------------------------------------------------------------

/// Clear the shown project's unread marks once it has stayed on screen for
/// `UNREAD_DWELL`, in an active window. Every visible pane counts, not only
/// the focused one. A newer switch cancels the pending clear.
pub(super) fn schedule_unread_clear(state: &State, workspace_id: &str) {
    let epoch = {
        let mut s = state.borrow_mut();
        s.unread_dwell_epoch = s.unread_dwell_epoch.wrapping_add(1);
        s.unread_dwell_epoch
    };
    let weak = Rc::downgrade(state);
    let workspace_id = workspace_id.to_string();
    glib::timeout_add_local_once(UNREAD_DWELL, move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let still_shown = {
            let s = state.borrow();
            s.unread_dwell_epoch == epoch
                && s.active_workspace()
                    .is_some_and(|workspace| workspace.id == workspace_id)
        };
        if still_shown {
            clear_visible_tab_unread(&state, &workspace_id);
        }
    });
}

// ---------------------------------------------------------------------------
// Session rows
// ---------------------------------------------------------------------------

fn agent_label(agent: &str) -> String {
    match agent {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        "gemini" => "Gemini".to_string(),
        "opencode" => "OpenCode".to_string(),
        other => other.to_string(),
    }
}

/// The rows of a project's session list: every terminal with `all`, else
/// only the active ones (working, waiting or unread).
fn session_row_views(s: &AppState, panes: &[gtk::Widget], all: bool) -> Vec<SessionRowView> {
    let mut views = Vec::new();
    let mut pane_index = 0;
    for pane_widget in panes {
        let Some(pane::PaneSessionTabs { pane_id, tabs }) = pane::session_tabs_in_pane(pane_widget)
        else {
            continue;
        };
        if tabs.is_empty() {
            continue;
        }
        for (index, tab) in tabs.into_iter().enumerate() {
            let activity = s.agent_activity.get(&tab.tab_id);
            let (glyph, state_label) = if tab.exited {
                (GLYPH_EXITED, "exited")
            } else {
                agent_activity::session_glyph(activity.map(|a| a.phase), tab.unread)
            };
            let phase = activity.map(|activity| activity.phase);
            let waiting = phase.is_some_and(AgentPhase::is_waiting);
            let working = phase == Some(AgentPhase::Working);
            let active = !tab.exited && (waiting || working || tab.unread);
            if !all && !active {
                continue;
            }
            let note = s.tab_notes.get(&tab.tab_id).map(|(text, _)| text.clone());
            let detail = if tab.exited {
                None
            } else if waiting || working {
                activity
                    .and_then(|activity| activity.detail.clone())
                    .or(note)
            } else if tab.unread {
                note
            } else {
                None
            };
            views.push(SessionRowView {
                pane_id,
                tab_id: tab.tab_id,
                title: tab.title,
                pane_start: all && pane_index > 0 && index == 0,
                background: all && !tab.active,
                shown: all && tab.active,
                glyph,
                state_label,
                agent: activity.map(|activity| agent_label(&activity.agent)),
                detail,
                active,
            });
        }
        pane_index += 1;
    }
    // Name the agent only when a project mixes agents.
    let agents: HashSet<&str> = views.iter().filter_map(|v| v.agent.as_deref()).collect();
    if agents.len() < 2 {
        for view in &mut views {
            view.agent = None;
        }
    }
    views
}

/// With the list closed and a single active terminal, its line goes under
/// the project name instead of a chip: what it does or asks. An unread one
/// needs nothing more, as the project row already shows its message.
fn inline_detail(views: &[SessionRowView]) -> Option<Option<(String, bool)>> {
    let [view] = views else {
        return None;
    };
    let waiting = matches!(view.state_label, "needs approval" | "needs input");
    let working = view.state_label == "working";
    Some(
        (waiting || working)
            .then(|| view.detail.clone().map(|detail| (detail, waiting)))
            .flatten(),
    )
}

/// Add the expand toggle and the (empty, hidden) session list to a project row.
pub(super) fn attach_session_list(
    state: &State,
    workspace_id: &str,
    row: &gtk::ListBoxRow,
) -> (gtk::Button, gtk::Box) {
    let toggle = gtk::Button::with_label("\u{25B8}");
    toggle.add_css_class("flat");
    toggle.add_css_class("limux-ws-sessions-toggle");
    toggle.set_focus_on_click(false);
    toggle.set_valign(gtk::Align::Center);
    toggle.set_tooltip_text(Some("Show terminals"));
    toggle.update_property(&[gtk::accessible::Property::Label("Show terminals")]);

    let sessions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sessions.add_css_class("limux-ws-sessions");
    sessions.set_visible(false);

    if let Some(vbox) = row.child().and_downcast::<gtk::Box>() {
        if let Some(top_row) = vbox.first_child().and_downcast::<gtk::Box>() {
            top_row.prepend(&toggle);
        }
        vbox.append(&sessions);
    }

    let state = state.clone();
    let workspace_id = workspace_id.to_string();
    toggle.connect_clicked(move |_| {
        {
            let mut s = state.borrow_mut();
            if !s.expanded_sessions.remove(&workspace_id) {
                s.expanded_sessions.insert(workspace_id.clone());
            }
        }
        refresh_session_rows(&state, &workspace_id, true);
    });
    (toggle, sessions)
}

/// Rebuild a project's session list when it is open and its rows changed
/// (or always with `force`), and hide it when it is closed.
pub(super) fn refresh_session_rows(state: &State, workspace_id: &str, force: bool) {
    let (expanded, toggle, container, panes, old) = {
        let s = state.borrow();
        let Some(workspace) = s
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        (
            s.expanded_sessions.contains(workspace_id),
            workspace.sessions_toggle.clone(),
            workspace.sessions_box.clone(),
            // Every pane, including ones a zoom keeps out of the root.
            workspace.split_container.panes(),
            s.session_views.get(workspace_id).cloned(),
        )
    };
    let (chevron, tip) = if expanded {
        ("\u{25BE}", "Show only active terminals")
    } else {
        ("\u{25B8}", "Show all terminals")
    };
    toggle.set_label(chevron);
    toggle.set_tooltip_text(Some(tip));
    toggle.update_property(&[gtk::accessible::Property::Label(tip)]);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);

    // Closed, the list still shows the active terminals, so a working or
    // waiting tab is visible under its project without opening anything.
    let views = session_row_views(&state.borrow(), &panes, expanded);
    if !force && old.as_ref() == Some(&views) {
        return;
    }
    let inline = if expanded {
        None
    } else {
        inline_detail(&views)
    };
    if !expanded && (views.is_empty() || inline == Some(None)) {
        container.set_visible(false);
        if let Some(old) = old {
            clear_outlines(&old);
            state.borrow_mut().session_views.remove(workspace_id);
            clear_children(&container);
        }
        return;
    }
    if let Some(old) = &old {
        clear_outlines(old);
    }
    clear_children(&container);
    if let Some(Some((detail, waiting))) = inline {
        container.add_css_class("limux-ws-sessions-inline");
        let line = gtk::Label::new(Some(&detail));
        line.set_xalign(0.0);
        line.set_ellipsize(gtk::pango::EllipsizeMode::End);
        line.add_css_class("limux-ws-activity-line");
        if waiting {
            line.add_css_class("limux-session-waiting");
        }
        line.set_tooltip_text(Some(&detail));
        container.append(&line);
        container.set_visible(true);
        state
            .borrow_mut()
            .session_views
            .insert(workspace_id.to_string(), views);
        return;
    }
    container.remove_css_class("limux-ws-sessions-inline");
    if views.is_empty() {
        let empty = gtk::Label::new(Some("No terminals"));
        empty.set_xalign(0.0);
        empty.add_css_class("limux-session-empty");
        container.append(&empty);
    }
    for view in &views {
        if view.pane_start {
            let gap = gtk::Separator::new(gtk::Orientation::Horizontal);
            gap.add_css_class("limux-session-pane-gap");
            container.append(&gap);
        }
        container.append(&build_session_row(state, workspace_id, view));
    }
    container.set_visible(true);
    state
        .borrow_mut()
        .session_views
        .insert(workspace_id.to_string(), views);
}

fn clear_children(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn clear_outlines(views: &[SessionRowView]) {
    for view in views {
        pane::set_pane_outlined(view.pane_id, false);
    }
}

fn build_session_row(state: &State, workspace_id: &str, view: &SessionRowView) -> gtk::Button {
    let glyph = gtk::Label::new(Some(view.glyph));
    glyph.add_css_class("limux-session-glyph");
    match view.state_label {
        "needs approval" | "needs input" => glyph.add_css_class("limux-session-waiting"),
        "unread" => glyph.add_css_class("limux-session-unread"),
        "working" => glyph.add_css_class("limux-session-working"),
        "exited" => {}
        // Nothing to act on: no sign. The terminal icon says what it is.
        _ => glyph.set_visible(false),
    }
    let title = gtk::Label::builder()
        .label(&view.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("limux-session-title");
    let icon = gtk::Image::from_icon_name("utilities-terminal-symbolic");
    icon.set_pixel_size(12);
    icon.add_css_class("limux-session-icon");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&icon);
    content.append(&title);
    if let Some(agent) = &view.agent {
        let agent = gtk::Label::new(Some(agent));
        agent.add_css_class("limux-session-agent");
        content.append(&agent);
    }
    content.append(&glyph);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 1);
    body.append(&content);
    if let Some(detail) = &view.detail {
        let line = gtk::Label::new(Some(detail));
        line.set_xalign(0.0);
        line.set_ellipsize(gtk::pango::EllipsizeMode::End);
        line.add_css_class("limux-session-detail");
        if view.state_label == "needs approval" || view.state_label == "needs input" {
            line.add_css_class("limux-session-waiting");
        }
        body.append(&line);
    }
    let button = gtk::Button::builder()
        .child(&body)
        .focus_on_click(false)
        .build();
    button.add_css_class("flat");
    button.add_css_class("limux-session-row");
    if view.background {
        button.add_css_class("limux-session-row-background");
    }
    if view.shown {
        button.add_css_class("limux-session-row-shown");
    }
    let mut spoken = match &view.agent {
        Some(agent) => format!("{}, {agent}, {}", view.title, view.state_label),
        None => format!("{}, {}", view.title, view.state_label),
    };
    if let Some(detail) = &view.detail {
        spoken = format!("{spoken}: {detail}");
    }
    button.set_tooltip_text(Some(&spoken));
    button.update_property(&[gtk::accessible::Property::Label(&spoken)]);

    let pane_id = view.pane_id;
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(move |_, _, _| pane::set_pane_outlined(pane_id, true));
    motion.connect_leave(move |_| pane::set_pane_outlined(pane_id, false));
    button.add_controller(motion);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(move |_| pane::set_pane_outlined(pane_id, true));
    focus.connect_leave(move |_| pane::set_pane_outlined(pane_id, false));
    button.add_controller(focus);

    let state = state.clone();
    let target = DesktopNotificationTarget {
        workspace_id: workspace_id.to_string(),
        pane_id: Some(view.pane_id),
        tab_id: Some(view.tab_id.clone()),
    };
    button.connect_clicked(move |_| {
        pane::set_pane_outlined(pane_id, false);
        activate_desktop_notification_target(&state, &target, None);
    });
    button
}

pub(super) fn agent_activity_css() -> &'static str {
    r#"
/* ---------- Agent activity ---------- */
.limux-notify-dot-waiting {
    font-weight: 700;
}
.limux-notify-dot-working {
    color: alpha(@window_fg_color, 0.65);
    font-size: 10px;
    margin-right: 6px;
}
.limux-sidebar-list row .limux-ws-colored .limux-notify-dot-working {
    color: alpha(white, 0.85);
}
.limux-indicator-waiting-dot {
    font-size: 9px;
}
.limux-ws-sessions-toggle {
    min-width: 16px;
    min-height: 16px;
    padding: 0;
    margin-right: 2px;
    font-size: 10px;
    color: alpha(@window_fg_color, 0.55);
}
.limux-ws-sessions {
    margin: 6px 0 2px 8px;
    padding: 2px 0 2px 8px;
    border-left: 1px solid alpha(@window_fg_color, 0.2);
}
/* Each terminal is a small tab: its own chip, normal weight (Adwaita makes
   button labels bold, which made them read as the project's name). */
.limux-session-row {
    min-height: 24px;
    padding: 2px 8px;
    margin: 2px 0;
    border-radius: 6px;
    background: alpha(@window_fg_color, 0.07);
}
.limux-session-row:hover {
    background: alpha(@window_fg_color, 0.14);
}
.limux-session-row label {
    font-weight: 400;
}
/* The tab on screen in its pane. */
.limux-session-row-shown {
    box-shadow: inset 2px 0 0 0 @accent_bg_color;
}
.limux-session-row-background .limux-session-title,
.limux-session-row-background .limux-session-icon {
    opacity: 0.55;
}
.limux-session-icon {
    color: alpha(@window_fg_color, 0.7);
}
.limux-session-glyph {
    min-width: 12px;
    font-size: 10px;
    color: alpha(@window_fg_color, 0.65);
}
.limux-session-title {
    font-size: 12px;
}
.limux-session-detail {
    font-size: 11px;
    margin-left: 18px;
    color: alpha(@window_fg_color, 0.6);
}
.limux-session-agent {
    font-size: 10px;
    color: alpha(@window_fg_color, 0.5);
}
.limux-session-working,
.limux-tab-agent-sign.limux-session-working {
    color: alpha(@window_fg_color, 0.55);
}
.limux-tab-agent-sign {
    font-size: 10px;
    margin-right: 3px;
}
/* One active terminal: its line sits under the project name, no chip. */
.limux-ws-sessions.limux-ws-sessions-inline {
    margin: 0;
    padding: 0;
    border-left: none;
}
.limux-ws-activity-line {
    font-size: 11px;
    margin-left: 20px;
    color: alpha(@window_fg_color, 0.62);
}
.limux-sidebar-list row .limux-ws-colored .limux-ws-activity-line {
    color: alpha(white, 0.8);
}
.limux-session-pane-gap {
    margin: 4px 0;
    background: alpha(@window_fg_color, 0.25);
    min-height: 1px;
}
.limux-session-empty {
    font-size: 11px;
    color: alpha(@window_fg_color, 0.4);
    margin-left: 6px;
}
.limux-ws-group-working {
    min-height: 0;
    min-width: 0;
    padding: 0 3px;
    margin-left: 0;
    color: alpha(white, 0.75);
    font-size: 11px;
}
.limux-ws-group-waiting {
    min-height: 0;
    min-width: 0;
    padding: 0 3px;
    margin-left: 0;
    font-size: 11px;
    font-weight: 700;
}
.limux-ws-group-more {
    min-height: 0;
    padding: 1px 10px;
    margin: 0 6px 0 22px;
    font-size: 11px;
    color: alpha(@window_fg_color, 0.65);
}
"#
}
