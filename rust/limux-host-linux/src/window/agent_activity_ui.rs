//! Live agent state in the window: the `agent.activity` control method, the
//! one signal each project row and top-bar pill shows, the expandable
//! session rows under a project, and the terminals kept in them.
//!
//! The rules live in `crate::agent_activity`; this module only reads panes,
//! applies events to `AppState::agent_activity` and paints widgets.

use super::*;
use crate::agent_activity::{
    self, ActivityCounts, AgentEvent, AgentPhase, ProjectSignal, GLYPH_UNREAD, GLYPH_WAITING,
    GLYPH_WORKING,
};

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
    /// When its current state began (Unix ms), for the age.
    since_ms: u64,
    /// Did something (working, waiting, unread, idle after work, exited):
    /// shown even while the list is closed, until the user closes it.
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

/// Whether a project shows `●`: one of its tabs is unread and no longer
/// working, or, with no unread tab, the project is marked unread itself. A
/// terminal that is still working keeps its spinner through a note (such as a
/// background agent finishing); its `●` shows once it stops. This is the
/// Agents panel's rule, so the counter and the group counts agree.
pub(super) fn has_finished_unread(
    s: &AppState,
    workspace_id: &str,
    workspace_unread: bool,
) -> bool {
    finished_unread(
        workspace_unread,
        pane::unread_tab_ids_in_workspace(workspace_id)
            .iter()
            .map(|tab_id| s.agent_activity.phase(tab_id) == Some(AgentPhase::Working)),
    )
}

/// `has_finished_unread` over the working flag of each unread tab.
fn finished_unread(
    workspace_unread: bool,
    unread_tabs_working: impl Iterator<Item = bool>,
) -> bool {
    let mut any_unread_tab = false;
    for working in unread_tabs_working {
        if !working {
            return true;
        }
        any_unread_tab = true;
    }
    workspace_unread && !any_unread_tab
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
        "limux-notify-dot-exited",
        "limux-notify-dot-quiet",
    ] {
        dot.remove_css_class(class);
    }
    let working = signal == ProjectSignal::Working;
    dot.set_visible(!working);
    workspace.row_extras.spinner.set_visible(working);
    workspace
        .row_extras
        .spinner
        .set_tooltip_text(agent_activity::counts_summary(counts).as_deref());
    // Waiting is the loudest row: a tinted card with an edge. Finished keeps
    // only its sign and bold name.
    if let Some(row_box) = workspace.sidebar_row.child() {
        if signal == ProjectSignal::Waiting {
            row_box.add_css_class("limux-sidebar-row-waiting");
        } else {
            row_box.remove_css_class("limux-sidebar-row-waiting");
        }
    }
    let (glyph, classes, tooltip): (&str, &[&str], Option<String>) = match signal {
        ProjectSignal::Waiting => (
            GLYPH_WAITING,
            &["limux-notify-dot", "limux-notify-dot-waiting"],
            agent_activity::counts_summary(counts),
        ),
        ProjectSignal::Unread if pane::workspace_has_exited_unread_tab(&workspace.id) => (
            GLYPH_EXITED,
            &["limux-notify-dot", "limux-notify-dot-exited"],
            Some("A terminal exited".to_string()),
        ),
        ProjectSignal::Unread => (
            GLYPH_UNREAD,
            &["limux-notify-dot"],
            Some("Unread".to_string()),
        ),
        // The spinner stands in for the dot.
        ProjectSignal::Working => (
            GLYPH_WORKING,
            &["limux-notify-dot-hidden"],
            agent_activity::counts_summary(counts),
        ),
        // Terminals that did something and are quiet now: a faint ring.
        ProjectSignal::None if counts.quiet > 0 => (
            agent_activity::GLYPH_IDLE,
            &["limux-notify-dot", "limux-notify-dot-quiet"],
            Some(format!(
                "{} quiet terminal{}",
                counts.quiet,
                if counts.quiet == 1 { "" } else { "s" }
            )),
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

/// Tell screen readers that a terminal now waits for the user.
fn announce_waiting(state: &State, tab_id: &str) {
    let (message, window) = {
        let s = state.borrow();
        let Some(activity) = s.agent_activity.get(tab_id) else {
            return;
        };
        if !activity.phase.is_waiting() {
            return;
        }
        let project = s
            .workspaces
            .iter()
            .find(|workspace| {
                pane::terminal_tab_ids_in_workspace(&workspace.id)
                    .iter()
                    .any(|id| id == tab_id)
            })
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        let what = match activity.phase {
            AgentPhase::NeedsInput => "has a question",
            _ => "needs approval",
        };
        let message = match &activity.detail {
            Some(detail) => format!("{project}: agent {what}: {detail}"),
            None => format!("{project}: agent {what}"),
        };
        (message, s.window.clone())
    };
    announce(window.upcast_ref(), &message);
}

/// `gtk_accessible_announce` (GTK 4.14) when the running GTK has it. Limux
/// builds against GTK 4.10, so it is looked up at run time; older GTK skips
/// the announcement.
fn announce(widget: &gtk::Widget, message: &str) {
    type AnnounceFn =
        unsafe extern "C" fn(*mut gtk::ffi::GtkAccessible, *const std::ffi::c_char, i32);
    static ANNOUNCE: std::sync::OnceLock<Option<AnnounceFn>> = std::sync::OnceLock::new();
    let announce = ANNOUNCE.get_or_init(|| {
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name only looks
        // up a symbol; a non-null result is GTK's function with this signature.
        let symbol =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"gtk_accessible_announce".as_ptr()) };
        (!symbol.is_null())
            .then(|| unsafe { std::mem::transmute::<*mut libc::c_void, AnnounceFn>(symbol) })
    });
    let (Some(announce), Ok(message)) = (announce, std::ffi::CString::new(message)) else {
        return;
    };
    // GTK_ACCESSIBLE_ANNOUNCEMENT_PRIORITY_MEDIUM
    const PRIORITY_MEDIUM: i32 = 1;
    use gtk::glib::translate::ToGlibPtr;
    let pointer: *mut gtk::ffi::GtkWidget = widget.to_glib_none().0;
    // SAFETY: every GtkWidget implements GtkAccessible; the widget and the
    // message live for the call.
    unsafe { announce(pointer.cast(), message.as_ptr(), PRIORITY_MEDIUM) };
}

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
    let was_waiting = state
        .borrow()
        .agent_activity
        .phase(&tab_id)
        .is_some_and(AgentPhase::is_waiting);
    let ended = event.phase.is_none();
    let changed = state.borrow_mut().agent_activity.apply(AgentEvent {
        tab_id: tab_id.clone(),
        ..event
    });
    if changed && ended {
        // Its exit screen and the shell's prompt coming back are not the
        // shell doing something: treat it like a shell starting.
        pane::note_shell_resumed(&tab_id);
    }
    if changed {
        refresh_activity(state);
        if !was_waiting {
            announce_waiting(state, &tab_id);
        }
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

/// Follow plain terminals from their own output (see
/// [`agent_activity::ActivityStore::track_terminal`]) and keep unread ones
/// listed. One that stops printing while off screen turns unread, like an
/// agent that finished. Returns whether anything changed.
fn track_terminals(state: &State) -> bool {
    let now = std::time::Instant::now();
    let now_ms = agent_activity::now_ms();
    let shown: HashSet<String> = shown_tab_ids(state).into_iter().collect();
    let ids: Vec<String> = state
        .borrow()
        .workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .collect();
    let mut changed = false;
    let mut finished = Vec::new();
    for workspace_id in ids {
        for terminal in pane::terminal_output_states(&workspace_id, now) {
            let since = terminal.working_since.map(|at| {
                let ago = u64::try_from(now.saturating_duration_since(at).as_millis())
                    .unwrap_or(u64::MAX);
                now_ms.saturating_sub(ago)
            });
            let mut s = state.borrow_mut();
            match s
                .agent_activity
                .track_terminal(&terminal.tab_id, now_ms, since)
            {
                agent_activity::TerminalChange::None => {}
                agent_activity::TerminalChange::Started => changed = true,
                agent_activity::TerminalChange::Finished => {
                    changed = true;
                    if !shown.contains(&terminal.tab_id) {
                        finished.push((workspace_id.clone(), terminal.pane_id, terminal.tab_id));
                        continue;
                    }
                }
            }
            if terminal.unread {
                changed |= s.agent_activity.keep(&terminal.tab_id, now_ms);
            }
        }
    }
    for (workspace_id, pane_id, tab_id) in finished {
        if pane::mark_tab_unread_in_workspace(&workspace_id, pane_id, &tab_id) == Some(true) {
            pending_panel::note_message(
                &mut state.borrow_mut(),
                &workspace_id,
                Some(&tab_id),
                "Finished",
            );
        }
    }
    changed
}

/// Close a kept terminal's entry: mark it read and drop it from the lists
/// until it does something again. Working and waiting ones stay.
pub(super) fn dismiss_terminal(state: &State, tab_id: &str) {
    let dropped = state.borrow_mut().agent_activity.dismiss(tab_id);
    if !dropped && state.borrow().agent_activity.get(tab_id).is_some() {
        return;
    }
    pane::clear_tab_unread_by_id(tab_id);
    state.borrow_mut().tab_notes.remove(tab_id);
    refresh_activity(state);
}

/// The tabs on screen: the active tab of each shown pane of the shown
/// project, in an active window.
fn shown_tab_ids(state: &State) -> Vec<String> {
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
    shown
}

/// Remember that the tabs on screen were seen now. Closed tabs are dropped.
fn note_seen_tabs(state: &State, now: u64) {
    let shown = shown_tab_ids(state);
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

/// The same sign in each terminal's tab (waiting or working), and a name for
/// untitled agent tabs: "Claude", "Claude 2" in project order.
fn sync_tab_signs(state: &State) {
    let s = state.borrow();
    let mut fallback_titles: HashMap<String, String> = HashMap::new();
    for workspace in &s.workspaces {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for tab_id in pane::terminal_tab_ids_in_workspace(&workspace.id) {
            let Some(activity) = s
                .agent_activity
                .get(&tab_id)
                .filter(|activity| activity.agent != agent_activity::TERMINAL_AGENT)
            else {
                continue;
            };
            let label = agent_label(&activity.agent);
            let n = seen.entry(label.clone()).or_insert(0);
            *n += 1;
            let title = if *n == 1 {
                label
            } else {
                format!("{label} {n}")
            };
            fallback_titles.insert(tab_id, title);
        }
    }
    pane::sync_tab_agent_signs(|tab_id| {
        // A plain terminal's tab already has its output spinner.
        let activity = s
            .agent_activity
            .get(tab_id)
            .filter(|activity| activity.agent != agent_activity::TERMINAL_AGENT)?;
        let (waiting, working, tooltip) = match activity.phase {
            AgentPhase::NeedsApproval => (true, false, "Needs your approval"),
            AgentPhase::NeedsInput => (true, false, "Has a question for you"),
            AgentPhase::Working => (false, true, "Agent working"),
            AgentPhase::Idle => (false, false, ""),
        };
        let tooltip = match &activity.detail {
            Some(detail) if waiting || working => format!("{tooltip}: {detail}"),
            _ => tooltip.to_string(),
        };
        Some(pane::TabAgentSign {
            waiting,
            working,
            tooltip,
            fallback_title: fallback_titles.get(tab_id).cloned(),
            detail: activity.detail.clone(),
        })
    });
}

/// The project name shown before the tabs while both bars are hidden.
pub(super) fn build_fullscreen_project() -> gtk::Label {
    let chip = gtk::Label::new(None);
    chip.add_css_class("limux-ws-group-pill");
    chip.add_css_class("limux-fullscreen-project");
    chip.set_valign(gtk::Align::Center);
    chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
    chip.set_max_width_chars(18);
    chip.set_visible(false);
    chip
}

/// Keep the full-screen project name current: the active project's name in
/// its group's colour. Only shown while it sits in a pane header.
pub(super) fn sync_fullscreen_project(state: &State) {
    let s = state.borrow();
    let chip = &s.fullscreen_project;
    if chip.parent().is_none() {
        return;
    }
    let Some(workspace) = s.active_workspace() else {
        chip.set_visible(false);
        return;
    };
    if chip.label() != workspace.name {
        chip.set_label(&workspace.name);
    }
    for color in WorkspaceColor::ALL {
        chip.remove_css_class(&sidebar_groups::group_color_class(color));
    }
    match s.sidebar.group_of(&workspace.id) {
        Some(group) => {
            chip.add_css_class(&sidebar_groups::group_color_class(group.color));
            chip.remove_css_class("limux-fullscreen-project-plain");
        }
        None => chip.add_css_class("limux-fullscreen-project-plain"),
    }
    chip.set_tooltip_text(Some(&format!("Project: {}", workspace.name)));
    chip.set_visible(true);
}

pub(super) fn install_activity_timer(state: &State) {
    let weak = Rc::downgrade(state);
    glib::timeout_add_local(ACTIVITY_REFRESH_INTERVAL, move || {
        let Some(state) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let tracked = track_terminals(&state);
        let settled = settle_quiet_activity(&state);
        if prune_activity(&state) || settled || tracked {
            refresh_activity(&state);
        } else {
            // Tab switches move the detail between tabs.
            sync_tab_signs(&state);
            sync_fullscreen_project(&state);
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
            let plain = activity.is_some_and(|a| a.agent == agent_activity::TERMINAL_AGENT);
            let (glyph, state_label) = if tab.exited {
                (GLYPH_EXITED, "exited")
            } else {
                match agent_activity::session_glyph(activity.map(|a| a.phase), tab.unread) {
                    (glyph, "waiting for a prompt") if plain => (glyph, "quiet"),
                    other => other,
                }
            };
            let phase = activity.map(|activity| activity.phase);
            let waiting = phase.is_some_and(AgentPhase::is_waiting);
            let working = phase == Some(AgentPhase::Working);
            // Anything that did something stays listed until it is closed.
            let active = activity.is_some() || tab.unread || tab.exited;
            if !all && !active {
                continue;
            }
            let note = s.tab_notes.get(&tab.tab_id);
            let detail = if waiting || working {
                activity
                    .and_then(|activity| activity.detail.clone())
                    .or_else(|| note.map(|(text, _)| text.clone()))
            } else if tab.unread || activity.is_some() {
                // What it last said, so a quiet one is still recognisable.
                note.map(|(text, _)| text.clone())
            } else {
                None
            };
            let since_ms = if tab.unread && !waiting && !working {
                note.map_or(0, |(_, at)| *at)
            } else {
                activity.map_or(0, |activity| activity.since_ms)
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
                agent: activity
                    .filter(|_| !plain)
                    .map(|activity| agent_label(&activity.agent)),
                detail,
                since_ms,
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

/// How loud a terminal's state is: waiting, then unread (and exited), then
/// working. The project row's line and age follow the loudest one.
fn urgency(view: &SessionRowView) -> u8 {
    match view.state_label {
        "needs approval" | "needs input" => 0,
        "unread" | "exited" => 1,
        "working" => 2,
        _ => 3,
    }
}

fn lead_view(views: &[SessionRowView]) -> Option<&SessionRowView> {
    views
        .iter()
        .filter(|view| view.active)
        .min_by_key(|view| urgency(view))
}

/// Finished or quiet terminals can be closed from the list; working, waiting
/// and exited ones say what is going on (an exited one closes in its pane).
fn is_closable_label(state_label: &str) -> bool {
    matches!(state_label, "unread" | "waiting for a prompt" | "quiet")
}

/// The ✕ at the end of a closable chip. A click on it closes the entry
/// instead of opening the terminal.
fn close_mark(state: &State, tab_id: &str) -> gtk::Label {
    let mark = gtk::Label::new(Some(GLYPH_EXITED));
    mark.add_css_class("limux-session-close");
    mark.set_tooltip_text(Some("Close from the list (Delete)"));
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    click.connect_pressed(|gesture, _, _, _| {
        // Keep the chip from opening the terminal.
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    {
        let state = state.clone();
        let tab_id = tab_id.to_string();
        click.connect_released(move |_, _, _, _| {
            let state = state.clone();
            let tab_id = tab_id.clone();
            // The chip goes away with it: rebuild after this event.
            glib::idle_add_local_once(move || dismiss_terminal(&state, &tab_id));
        });
    }
    mark.add_controller(click);
    mark
}

fn is_waiting_label(state_label: &str) -> bool {
    matches!(state_label, "needs approval" | "needs input")
}

/// Widgets added to a project row for agent activity.
#[derive(Clone)]
pub(super) struct RowExtras {
    /// Shown in the sign column instead of the dot while an agent works.
    pub spinner: gtk::Spinner,
    /// "Bash: cargo test" after the name, in a collapsed group's preview.
    pub preview_label: gtk::Label,
    /// How long the current state has lasted ("3m").
    pub age_label: gtk::Label,
}

/// A small spinner for "working" (it stands still when animations are off).
pub(super) fn working_spinner() -> gtk::Spinner {
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(10, 10);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_halign(gtk::Align::Center);
    spinner.set_can_target(false);
    spinner.add_css_class("limux-working-spinner");
    spinner.start();
    spinner
}

/// Rework a project row for agent activity: the sign (dot or spinner) gets a
/// fixed column before the name, the terminal count ("3 ▾") and the age go
/// after it, and the (empty, hidden) session list goes below.
pub(super) fn attach_session_list(
    state: &State,
    workspace_id: &str,
    row: &gtk::ListBoxRow,
) -> (gtk::Button, gtk::Box, RowExtras) {
    let toggle = gtk::Button::with_label("");
    toggle.add_css_class("flat");
    toggle.add_css_class("limux-ws-sessions-toggle");
    toggle.set_focus_on_click(false);
    toggle.set_valign(gtk::Align::Center);
    toggle.set_visible(false);

    let sessions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sessions.add_css_class("limux-ws-sessions");
    sessions.set_visible(false);

    let spinner = working_spinner();
    spinner.set_visible(false);
    let preview_label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .visible(false)
        .build();
    preview_label.add_css_class("limux-ws-preview-detail");
    let age_label = gtk::Label::new(None);
    age_label.add_css_class("limux-ws-age");
    age_label.set_valign(gtk::Align::Center);
    age_label.set_visible(false);

    if let Some(vbox) = row.child().and_downcast::<gtk::Box>() {
        if let Some(top_row) = vbox.first_child().and_downcast::<gtk::Box>() {
            // [sign][name][preview][3 ▾][age][star][close]
            let dot = top_row.first_child();
            let name = dot.as_ref().and_then(|dot| dot.next_sibling());
            let gutter = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            gutter.add_css_class("limux-ws-sign-gutter");
            gutter.set_size_request(18, -1);
            gutter.set_halign(gtk::Align::Start);
            // Its children centre themselves; the column itself never grows.
            gutter.set_hexpand(false);
            if let Some(dot) = &dot {
                top_row.remove(dot);
                dot.set_halign(gtk::Align::Center);
                dot.set_hexpand(true);
                gutter.append(dot);
            }
            spinner.set_hexpand(true);
            gutter.append(&spinner);
            top_row.prepend(&gutter);
            if let Some(name) = &name {
                // The preview shares the line: the name only takes what it needs.
                top_row.insert_child_after(&preview_label, Some(name));
                top_row.insert_child_after(&toggle, Some(&preview_label));
                top_row.insert_child_after(&age_label, Some(&toggle));
            }
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
    (
        toggle,
        sessions,
        RowExtras {
            spinner,
            preview_label,
            age_label,
        },
    )
}

/// Refresh a project's agent line: the terminal count button, the age, the
/// one-line preview (in a collapsed group) or the line and chips below the
/// name. Chips rebuild only when their rows changed (or always with `force`).
pub(super) fn refresh_session_rows(state: &State, workspace_id: &str, force: bool) {
    let Some((expanded, toggle, container, panes, old, extras, notify_label, preview, name_label)) =
        ({
            let s = state.borrow();
            s.workspaces
                .iter()
                .find(|workspace| workspace.id == workspace_id)
                .map(|workspace| {
                    (
                        s.expanded_sessions.contains(workspace_id),
                        workspace.sessions_toggle.clone(),
                        workspace.sessions_box.clone(),
                        // Every pane, including ones a zoom keeps out of the root.
                        workspace.split_container.panes(),
                        s.session_views.get(workspace_id).cloned(),
                        workspace.row_extras.clone(),
                        workspace.notify_label.clone(),
                        s.preview_rows.contains(workspace_id),
                        workspace.name_label.clone(),
                    )
                })
        })
    else {
        return;
    };

    let all_views = session_row_views(&state.borrow(), &panes, true);
    let terminals = all_views.len();
    // The list only opens with two or more terminals.
    let expanded = expanded && terminals >= 2 && !preview;
    let label = format!(
        "{terminals} {}",
        if expanded { "\u{25B4}" } else { "\u{25BE}" }
    );
    let tip = if expanded {
        "Show only active terminals"
    } else {
        "Show all terminals"
    };
    toggle.set_label(&label);
    toggle.set_tooltip_text(Some(tip));
    toggle.update_property(&[gtk::accessible::Property::Label(&format!(
        "{terminals} terminals. {tip}"
    ))]);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
    toggle.set_visible(terminals >= 2 && !preview);

    let lead = lead_view(&all_views).cloned();
    let now = agent_activity::now_ms();
    let age = lead
        .as_ref()
        .map(|lead| agent_activity::ago_label(now, lead.since_ms))
        .unwrap_or_default();
    extras.age_label.set_label(&age);
    extras.age_label.set_visible(!age.is_empty());

    // In a collapsed group the project is one line: sign, name, what its
    // loudest terminal does or says, and how long ago.
    let preview_text = lead.as_ref().and_then(|lead| {
        lead.detail.clone().or_else(|| {
            (!notify_label.label().is_empty()).then(|| notify_label.label().to_string())
        })
    });
    extras
        .preview_label
        .set_label(preview_text.as_deref().unwrap_or_default());
    extras
        .preview_label
        .set_visible(preview && preview_text.is_some());
    // In a preview the name takes only what it needs; the line gets the rest.
    let one_line = preview && preview_text.is_some();
    name_label.set_hexpand(!one_line);
    name_label.set_max_width_chars(if one_line { 12 } else { -1 });
    // Keep a short name whole; the detail is what gets cut.
    let name_chars = i32::try_from(name_label.label().chars().count()).unwrap_or(i32::MAX);
    name_label.set_width_chars(if one_line { name_chars.min(12) } else { -1 });
    if let Some(row_box) = container.parent() {
        if preview {
            row_box.add_css_class("limux-ws-preview");
        } else {
            row_box.remove_css_class("limux-ws-preview");
        }
    }
    let unread_row = container
        .parent()
        .is_some_and(|row_box| row_box.has_css_class("limux-sidebar-row-unread"));
    notify_label.set_visible(!preview && unread_row && !notify_label.label().is_empty());
    if preview {
        container.set_visible(false);
        if let Some(old) = state.borrow_mut().session_views.remove(workspace_id) {
            clear_outlines(&old);
            clear_children(&container);
        }
        return;
    }

    let views: Vec<SessionRowView> = if expanded {
        all_views
    } else {
        session_row_views(&state.borrow(), &panes, false)
    };
    if !force && old.as_ref() == Some(&views) {
        refresh_chip_ages(&container, &views, now);
        return;
    }
    // One active terminal: its line goes under the project name, no chip. An
    // unread one needs nothing more, as the row already shows its message.
    let inline = match (expanded, views.as_slice()) {
        (false, [view]) => Some(
            (view.state_label == "working" || is_waiting_label(view.state_label))
                .then(|| {
                    view.detail
                        .clone()
                        .map(|d| (d, is_waiting_label(view.state_label)))
                })
                .flatten(),
        ),
        _ => None,
    };
    if let Some(old) = &old {
        clear_outlines(old);
    }
    clear_children(&container);
    if !expanded && (views.is_empty() || inline == Some(None)) {
        container.set_visible(false);
        state
            .borrow_mut()
            .session_views
            .insert(workspace_id.to_string(), views);
        return;
    }
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
    } else {
        container.remove_css_class("limux-ws-sessions-inline");
        for view in &views {
            if view.pane_start {
                let gap = gtk::Separator::new(gtk::Orientation::Horizontal);
                gap.add_css_class("limux-session-pane-gap");
                container.append(&gap);
            }
            container.append(&build_session_row(state, workspace_id, view, now));
        }
    }
    container.set_visible(true);
    state
        .borrow_mut()
        .session_views
        .insert(workspace_id.to_string(), views);
}

/// Update the chips' ages in place (they change every minute).
fn refresh_chip_ages(container: &gtk::Box, views: &[SessionRowView], now: u64) {
    let mut child = container.first_child();
    let mut index = 0;
    while let Some(widget) = child {
        if widget.has_css_class("limux-session-row") {
            if let (Some(view), Some(age)) = (
                views.get(index),
                widget_tree_find(&widget, "limux-session-age"),
            ) {
                age.set_label(&agent_activity::ago_label(now, view.since_ms));
            }
            index += 1;
        }
        child = widget.next_sibling();
    }
}

fn widget_tree_find(root: &gtk::Widget, css_class: &str) -> Option<gtk::Label> {
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget.has_css_class(css_class) {
            return widget.downcast::<gtk::Label>().ok();
        }
        if let Some(found) = widget_tree_find(&widget, css_class) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
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

/// One terminal as a small tab: the sign in the same column as the project's,
/// its name and age, and what it does or asks below.
fn build_session_row(
    state: &State,
    workspace_id: &str,
    view: &SessionRowView,
    now: u64,
) -> gtk::Button {
    let waiting = is_waiting_label(view.state_label);
    let gutter = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    gutter.set_size_request(18, -1);
    gutter.set_hexpand(false);
    gutter.add_css_class("limux-session-gutter");
    if view.state_label == "working" {
        let spinner = working_spinner();
        spinner.set_hexpand(true);
        gutter.append(&spinner);
    } else {
        let glyph = gtk::Label::new(Some(view.glyph));
        glyph.add_css_class("limux-session-glyph");
        glyph.set_hexpand(true);
        match view.state_label {
            "needs approval" | "needs input" => glyph.add_css_class("limux-session-waiting"),
            "unread" => glyph.add_css_class("limux-session-unread"),
            "exited" => glyph.add_css_class("limux-session-exited"),
            "waiting for a prompt" | "quiet" => glyph.add_css_class("limux-session-quiet"),
            // A terminal that never did anything: no sign.
            _ => glyph.set_visible(false),
        }
        gutter.append(&glyph);
    }
    let title = gtk::Label::builder()
        .label(&view.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("limux-session-title");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&gutter);
    content.append(&title);
    if let Some(agent) = &view.agent {
        let agent = gtk::Label::new(Some(agent));
        agent.add_css_class("limux-session-agent");
        content.append(&agent);
    }
    let age = gtk::Label::new(Some(&agent_activity::ago_label(now, view.since_ms)));
    age.add_css_class("limux-session-age");
    content.append(&age);
    let closable = is_closable_label(view.state_label);
    if closable {
        content.append(&close_mark(state, &view.tab_id));
    }

    let body = gtk::Box::new(gtk::Orientation::Vertical, 1);
    body.append(&content);
    if let Some(detail) = &view.detail {
        let line = gtk::Label::new(Some(detail));
        line.set_xalign(0.0);
        line.add_css_class("limux-session-detail");
        if waiting {
            // A question is read in full.
            line.set_wrap(true);
            line.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            line.set_lines(3);
            line.set_ellipsize(gtk::pango::EllipsizeMode::End);
        } else {
            line.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        body.append(&line);
    }
    let button = gtk::Button::builder()
        .child(&body)
        .focus_on_click(false)
        .build();
    button.add_css_class("flat");
    button.add_css_class("limux-session-row");
    if waiting {
        button.add_css_class("limux-session-row-waiting");
    }
    if matches!(
        view.state_label,
        "waiting for a prompt" | "quiet" | "terminal"
    ) {
        button.add_css_class("limux-session-row-quiet");
    }
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

    if closable {
        let keys = gtk::EventControllerKey::new();
        let state = state.clone();
        let tab_id = view.tab_id.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                dismiss_terminal(&state, &tab_id);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        button.add_controller(keys);
    }

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
/* One sign column for rows, previews and chips. */
.limux-ws-sign-gutter label.limux-notify-dot,
.limux-ws-sign-gutter label.limux-notify-dot-hidden {
    font-size: 12px;
    margin: 0;
}
.limux-notify-dot-waiting {
    font-weight: 700;
}
.limux-notify-dot-exited {
    font-weight: 700;
}
.limux-working-spinner {
    color: alpha(@window_fg_color, 0.62);
    -gtk-icon-size: 11px;
    min-width: 11px;
    min-height: 11px;
}
.limux-sidebar-list row .limux-ws-colored .limux-working-spinner {
    color: alpha(white, 0.85);
}
.limux-indicator-waiting-dot {
    font-size: 9px;
}
/* "3 ▾": the terminals of a project, shown with two or more. */
.limux-ws-sessions-toggle {
    min-width: 0;
    min-height: 0;
    padding: 0 5px;
    margin-left: 6px;
    border-radius: 5px;
    font-size: 10.5px;
    color: alpha(@window_fg_color, 0.6);
    border: 1px solid alpha(@window_fg_color, 0.2);
}
.limux-ws-age,
.limux-session-age {
    margin-left: 6px;
    font-size: 10.5px;
    font-feature-settings: "tnum";
    color: alpha(@window_fg_color, 0.55);
}
.limux-ws-sessions {
    margin: 5px 0 1px 8px;
    padding: 0 0 0 7px;
    border-left: 1px solid alpha(@window_fg_color, 0.2);
}
/* One active terminal: its line sits under the project name, no chip. */
.limux-ws-sessions.limux-ws-sessions-inline {
    margin: 0;
    padding: 0;
    border-left: none;
}
.limux-ws-activity-line {
    font-size: 12px;
    margin-left: 18px;
    color: alpha(@window_fg_color, 0.74);
}
.limux-sidebar-row-waiting .limux-ws-activity-line {
    color: @window_fg_color;
}
.limux-sidebar-list row .limux-ws-colored .limux-ws-activity-line {
    color: alpha(white, 0.85);
}
/* A collapsed group's member: one line. */
.limux-sidebar-row-box.limux-ws-preview {
    padding: 2px 6px 2px 4px;
    margin-top: 0;
    margin-bottom: 0;
}
.limux-ws-preview .limux-ws-name {
    font-size: 12px;
    font-weight: 600;
}
.limux-ws-preview-detail {
    margin-left: 7px;
    font-size: 11.5px;
    color: alpha(@window_fg_color, 0.66);
}
.limux-sidebar-row-waiting .limux-ws-preview-detail {
    color: @window_fg_color;
}
/* Each terminal is a small tab: its own chip with a top edge. Adwaita makes
   button labels bold, so the weight is set here. */
.limux-session-row {
    min-height: 24px;
    padding: 3px 7px 4px 0;
    margin: 3px 0;
    border-radius: 6px 6px 3px 3px;
    background: alpha(@window_fg_color, 0.07);
    border-top: 1.5px solid alpha(@window_fg_color, 0.16);
}
.limux-session-row:hover {
    background: alpha(@window_fg_color, 0.14);
}
.limux-session-row label {
    font-weight: 400;
}
.limux-session-row label.limux-session-title {
    font-size: 12px;
    font-weight: 600;
}
.limux-session-row-quiet label.limux-session-title {
    font-weight: 400;
    color: alpha(@window_fg_color, 0.7);
}
/* In the full list: the tab on screen, and dimmed background ones. */
.limux-session-row.limux-session-row-shown {
    border-top-color: @accent_bg_color;
    background: alpha(@window_fg_color, 0.15);
}
.limux-session-row-background .limux-session-title {
    opacity: 0.55;
}
.limux-session-glyph {
    font-size: 12px;
    color: alpha(@window_fg_color, 0.65);
}
.limux-session-exited {
    font-weight: 700;
}
/* Did something earlier and is quiet now: kept until closed. */
.limux-session-glyph.limux-session-quiet {
    color: alpha(@window_fg_color, 0.45);
}
.limux-session-close {
    margin-left: 6px;
    padding: 0 3px;
    border-radius: 4px;
    font-size: 10px;
    color: alpha(@window_fg_color, 0.45);
}
.limux-session-close:hover {
    color: @window_fg_color;
    background: alpha(@window_fg_color, 0.15);
}
.limux-session-detail {
    font-size: 12px;
    margin-left: 18px;
    color: alpha(@window_fg_color, 0.74);
}
.limux-session-row-waiting .limux-session-detail {
    color: @window_fg_color;
}
.limux-session-agent {
    font-size: 10px;
    color: alpha(@window_fg_color, 0.5);
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
/* Group header counts: loudest first, on the right. */
.limux-ws-group-working,
.limux-ws-group-waiting {
    min-height: 20px;
    min-width: 0;
    padding: 0 5px;
    margin-left: 0;
    font-size: 12px;
    font-weight: 700;
}
.limux-ws-group-working {
    color: alpha(white, 0.75);
}
.limux-ws-group-more {
    min-height: 0;
    padding: 1px 8px;
    margin: 0 6px 0 22px;
    font-size: 11.5px;
    color: alpha(@window_fg_color, 0.66);
}
/* Terminal tabs: the agent's sign, and what a background one does. */
.limux-tab-agent-sign {
    font-size: 12px;
    margin-right: 4px;
}
.limux-tab-agent-detail {
    margin-left: 6px;
    font-size: 11.5px;
    color: alpha(@window_fg_color, 0.55);
}
/* Full screen: which project this is. */
.limux-fullscreen-project {
    margin: 0 6px 0 2px;
    padding: 1px 9px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 700;
    color: white;
}
.limux-fullscreen-project.limux-fullscreen-project-plain {
    background: alpha(@window_fg_color, 0.1);
    color: @window_fg_color;
}
"#
}

#[cfg(test)]
mod tests {
    use super::finished_unread;

    #[test]
    fn a_project_note_does_not_hide_a_working_unread_terminal() {
        // A project note alone shows `●`.
        assert!(finished_unread(true, [].into_iter()));
        // With an unread terminal still working, the project keeps its
        // spinner, as the Agents panel counts it under Working.
        assert!(!finished_unread(true, [true].into_iter()));
        assert!(!finished_unread(false, [true].into_iter()));
        // Once one unread terminal stops, the project shows `●`.
        assert!(finished_unread(true, [true, false].into_iter()));
        assert!(finished_unread(false, [false].into_iter()));
        assert!(!finished_unread(false, [].into_iter()));
    }
}
