//! The pending panel: one counter in the toolbar (`◆ N` waiting, `● N` unread
//! and a dimmed `↻ N` working, each shown when non-zero, hidden at zero) and a
//! popover that lists every terminal waiting for the user or unread, so
//! finished work is one click away even with every group collapsed or the
//! sidebar hidden. `Ctrl+Alt+P` opens it from anywhere.
//!
//! It reads the same state as the project rows, pills and group headers (agent
//! activity and unread marks); it has no attention model of its own. Unread
//! clears when the user types into the terminal (or marks it read), waiting
//! when the agent continues. Terminals that did something stay listed as
//! quiet until they are closed here or in the sidebar.

use super::*;
use crate::agent_activity::{
    self, PendingKind, GLYPH_EXITED, GLYPH_UNREAD, GLYPH_WAITING, GLYPH_WORKING,
};

/// Entries that get a number key (1-9), counted from the top.
const NUMBERED_ENTRIES: usize = 9;

/// Rows visible before the list scrolls.
const VISIBLE_ROWS: i32 = 8;
const ROW_HEIGHT: i32 = 50;

/// The popover that is open now, if any, and what it lists.
pub(super) struct OpenPanel {
    popover: gtk::Popover,
    list: gtk::ListBox,
    mark_all: gtk::Button,
    clear_quiet: gtk::Button,
    entries: Vec<PendingEntry>,
    /// Each entry's age label with its time, refreshed while the panel is open.
    ages: Vec<(gtk::Label, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingEntry {
    kind: PendingKind,
    at_ms: u64,
    workspace_id: String,
    workspace_name: String,
    group_color: Option<WorkspaceColor>,
    pane_id: Option<u32>,
    tab_id: Option<String>,
    title: String,
    /// A plain terminal, not an agent.
    plain: bool,
    message: String,
    /// A held terminal whose process ended.
    exited: bool,
    /// It ended with a non-zero code or a signal.
    failed: bool,
}

/// The toolbar counter. Placed by `apply_top_bar_mode` next to the folder
/// button, or beside the sidebar toggle when both bars are hidden.
pub(super) fn build_counter() -> gtk::Button {
    let counter = gtk::Button::new();
    // ◆ waiting, ✕ exited unseen (rose when one failed), ● unread, ↻ working
    // (dimmed), in that order.
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    for class in [
        "limux-pending-count-waiting",
        "limux-pending-count-exited",
        "limux-pending-count-unread",
        "limux-pending-count-working",
    ] {
        let label = gtk::Label::new(None);
        label.add_css_class(class);
        label.set_visible(false);
        content.append(&label);
    }
    counter.set_child(Some(&content));
    counter.add_css_class("flat");
    counter.add_css_class("limux-pending-counter");
    counter.set_focus_on_click(false);
    counter.set_valign(gtk::Align::Center);
    counter.set_visible(false);
    counter
}

pub(super) fn install(state: &State, counter: &gtk::Button) {
    let state = state.clone();
    counter.connect_clicked(move |_| toggle(&state));
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// Remember the latest message a terminal (or a project without a terminal
/// target) sent, for the panel's second line and its age.
pub(super) fn note_message(
    s: &mut AppState,
    workspace_id: &str,
    tab_id: Option<&str>,
    message: &str,
) {
    let note = (message.to_string(), agent_activity::now_ms());
    match tab_id {
        Some(tab_id) => {
            s.tab_notes.insert(tab_id.to_string(), note);
        }
        None => {
            s.workspace_notes.insert(workspace_id.to_string(), note);
        }
    }
}

fn pending_entries(state: &State) -> Vec<PendingEntry> {
    let s = state.borrow();
    let mut entries = Vec::new();
    for workspace in &s.workspaces {
        let group_color = s.sidebar.group_of(&workspace.id).map(|group| group.color);
        // Every pane, including ones a zoom keeps out of the root.
        let panes = workspace.split_container.panes();
        let mut any_tab_unread = false;
        for pane_widget in &panes {
            let Some(pane::PaneSessionTabs { pane_id, tabs }) =
                pane::session_tabs_in_pane(pane_widget)
            else {
                continue;
            };
            for tab in tabs {
                any_tab_unread |= tab.unread;
                let activity = s.agent_activity.get(&tab.tab_id);
                let waiting = activity.is_some_and(|activity| activity.phase.is_waiting());
                let working = activity
                    .is_some_and(|activity| activity.phase == agent_activity::AgentPhase::Working);
                let plain = activity
                    .is_some_and(|activity| activity.agent == agent_activity::TERMINAL_AGENT);
                if !waiting && !tab.unread && !working && activity.is_none() {
                    continue;
                }
                let note = s.tab_notes.get(&tab.tab_id);
                let (kind, at_ms, message) = if waiting {
                    let fallback = match activity.map(|activity| activity.phase) {
                        Some(agent_activity::AgentPhase::NeedsInput) => "Has a question",
                        _ => "Needs approval",
                    };
                    // The question or command itself when the agent said it,
                    // as on the project row.
                    let message = activity
                        .and_then(|activity| activity.detail.clone())
                        .or_else(|| note.map(|(text, _)| text.clone()))
                        .unwrap_or_else(|| fallback.to_string());
                    (
                        PendingKind::Waiting,
                        activity.map_or(0, |activity| activity.since_ms),
                        message,
                    )
                } else if working {
                    // Still working: a note it got shows once it stops.
                    (
                        PendingKind::Working,
                        activity.map_or(0, |activity| activity.since_ms),
                        activity
                            .and_then(|activity| activity.detail.clone())
                            .unwrap_or_else(|| {
                                if plain { "Running" } else { "Working" }.to_string()
                            }),
                    )
                } else if !tab.unread {
                    (
                        PendingKind::Quiet,
                        activity.map_or(0, |activity| activity.since_ms),
                        note.map(|(text, _)| text.clone()).unwrap_or_else(|| {
                            if plain {
                                "Quiet"
                            } else {
                                "Waiting for a prompt"
                            }
                            .to_string()
                        }),
                    )
                } else {
                    (
                        PendingKind::Unread,
                        note.map_or(0, |(_, at)| *at),
                        note.map_or("Unread".to_string(), |(text, _)| text.clone()),
                    )
                };
                entries.push(PendingEntry {
                    kind,
                    at_ms,
                    workspace_id: workspace.id.clone(),
                    workspace_name: workspace.name.clone(),
                    group_color,
                    pane_id: Some(pane_id),
                    tab_id: Some(tab.tab_id),
                    // The default name says nothing next to the project.
                    title: if tab.title == pane::DEFAULT_TERMINAL_TITLE {
                        String::new()
                    } else {
                        tab.title
                    },
                    plain,
                    message,
                    exited: tab.exited,
                    failed: tab.failed,
                });
            }
        }
        // A project marked unread without a terminal target is one entry.
        if (workspace.unread || workspace.manual_unread) && !any_tab_unread {
            let note = s.workspace_notes.get(&workspace.id);
            entries.push(PendingEntry {
                kind: PendingKind::Unread,
                at_ms: note.map_or(0, |(_, at)| *at),
                workspace_id: workspace.id.clone(),
                workspace_name: workspace.name.clone(),
                group_color,
                pane_id: None,
                tab_id: None,
                title: String::new(),
                plain: false,
                exited: false,
                failed: false,
                message: note.map_or_else(
                    || {
                        if workspace.manual_unread {
                            "Marked unread".to_string()
                        } else {
                            workspace.notify_label.label().to_string()
                        }
                    },
                    |(text, _)| text.clone(),
                ),
            });
        }
    }
    let mut keyed: Vec<(PendingKind, u64, PendingEntry)> = entries
        .into_iter()
        .map(|entry| (entry.kind, entry.at_ms, entry))
        .collect();
    agent_activity::sort_pending(&mut keyed);
    keyed.into_iter().map(|(_, _, entry)| entry).collect()
}

// ---------------------------------------------------------------------------
// Counter
// ---------------------------------------------------------------------------

/// Refresh the counter, and the open panel when its entries changed. Called
/// whenever unread marks or agent states change.
pub(super) fn sync(state: &State) {
    let Some(counter) = state.borrow().top_bar_pending_btn.clone() else {
        return;
    };
    let entries = pending_entries(state);
    let count = |kind| entries.iter().filter(|entry| entry.kind == kind).count();
    // An unseen exit counts as ✕, not as ●, as on its project row.
    let exits = || {
        entries
            .iter()
            .filter(|entry| entry.kind == PendingKind::Unread && entry.exited)
    };
    let (exited, failed) = (
        exits().count(),
        exits().filter(|entry| entry.failed).count(),
    );
    let (waiting, unread, working) = (
        count(PendingKind::Waiting),
        entries
            .iter()
            .filter(|entry| entry.kind == PendingKind::Unread && !entry.exited)
            .count(),
        count(PendingKind::Working),
    );
    // Quiet terminals are listed in the panel but not counted. While the
    // panel is open the counter stays: it anchors the panel, and closing the
    // last counted entry must not close the list of quiet ones with it.
    let panel_open = state.borrow().pending_open.is_some();
    if waiting + exited + unread + working == 0 && !panel_open {
        counter.set_visible(false);
    } else if waiting + exited + unread + working == 0 {
        let mut child = counter.child().and_then(|content| content.first_child());
        while let Some(label) = child.and_downcast::<gtk::Label>() {
            label.set_visible(false);
            child = label.next_sibling();
        }
        counter.remove_css_class("limux-pending-counter-waiting");
        counter.remove_css_class("limux-pending-counter-failed");
        counter.set_tooltip_text(Some("Nothing pending (Ctrl+Alt+P)"));
    } else {
        let parts = [
            (waiting > 0).then(|| format!("{GLYPH_WAITING} {waiting}")),
            (exited > 0).then(|| format!("{GLYPH_EXITED} {exited}")),
            (unread > 0).then(|| format!("{GLYPH_UNREAD} {unread}")),
            (working > 0).then(|| format!("{GLYPH_WORKING} {working}")),
        ];
        let mut child = counter.child().and_then(|content| content.first_child());
        for part in parts {
            let Some(label) = child.and_downcast::<gtk::Label>() else {
                break;
            };
            label.set_label(part.as_deref().unwrap_or_default());
            label.set_visible(part.is_some());
            child = label.next_sibling();
        }
        if waiting > 0 {
            counter.add_css_class("limux-pending-counter-waiting");
        } else {
            counter.remove_css_class("limux-pending-counter-waiting");
        }
        if failed > 0 {
            counter.add_css_class("limux-pending-counter-failed");
        } else {
            counter.remove_css_class("limux-pending-counter-failed");
        }
        let tip = summary_text(waiting, exited, failed, unread, working);
        counter.set_tooltip_text(Some(&format!("{tip} (Ctrl+Alt+P)")));
        counter.update_property(&[gtk::accessible::Property::Label(&format!("Pending: {tip}"))]);
        counter.set_visible(true);
    }

    let open = state.borrow().pending_open.as_ref().map(|open| {
        (
            open.entries != entries,
            open.list.clone(),
            open.mark_all.clone(),
            open.clear_quiet.clone(),
        )
    });
    if let Some((changed, list, mark_all, clear_quiet)) = open {
        if changed {
            let selected = list.selected_row().map(|row| row.index());
            let ages = fill_list(&list, &mark_all, &clear_quiet, &entries);
            reselect(&list, selected);
            if let Some(open) = state.borrow_mut().pending_open.as_mut() {
                open.entries = entries;
                open.ages = ages;
            }
        } else {
            refresh_ages(state);
        }
    }
}

/// Keep "2m" style ages current while the list itself is unchanged.
fn refresh_ages(state: &State) {
    let s = state.borrow();
    let Some(open) = s.pending_open.as_ref() else {
        return;
    };
    let now = agent_activity::now_ms();
    for (label, at_ms) in &open.ages {
        let text = agent_activity::ago_label(now, *at_ms);
        if label.label() != text {
            label.set_label(&text);
        }
    }
}

fn summary_text(
    waiting: usize,
    exited: usize,
    failed: usize,
    unread: usize,
    working: usize,
) -> String {
    let mut parts = Vec::new();
    if waiting > 0 {
        parts.push(format!("{waiting} waiting for you"));
    }
    if exited > 0 {
        parts.push(agent_activity::exited_summary(exited, failed));
    }
    if unread > 0 {
        parts.push(format!("{unread} unread"));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if parts.is_empty() {
        "No agents".to_string()
    } else {
        parts.join(", ")
    }
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

/// Open the panel, or close it when it is open (the counter and Ctrl+Alt+P).
pub(super) fn toggle(state: &State) {
    let open = state
        .borrow()
        .pending_open
        .as_ref()
        .map(|open| open.popover.clone());
    match open {
        Some(popover) => popover.popdown(),
        None => open_panel(state),
    }
}

/// Where the popover hangs: the counter when it is shown, else the folder or
/// sidebar-toggle button, else (both bars hidden, full screen) the top-left
/// corner of the terminals.
fn anchor(state: &State) -> Option<(gtk::Widget, Option<gtk::gdk::Rectangle>)> {
    let s = state.borrow();
    let mapped = |widget: Option<gtk::Widget>| widget.filter(|widget| widget.is_mapped());
    if let Some(widget) = mapped(s.top_bar_pending_btn.clone().map(|button| button.upcast()))
        .or_else(|| {
            mapped(
                s.top_bar_open_folder_btn_ref
                    .clone()
                    .map(|button| button.upcast()),
            )
        })
        .or_else(|| mapped(s.sidebar_toggle.clone().map(|button| button.upcast())))
    {
        return Some((widget, None));
    }
    let root = s.active_workspace()?.root.clone();
    root.is_mapped()
        .then(|| (root, Some(gtk::gdk::Rectangle::new(16, 4, 1, 1))))
}

fn open_panel(state: &State) {
    let Some((parent, pointing_to)) = anchor(state) else {
        return;
    };

    let title = gtk::Label::new(Some("Agents"));
    title.add_css_class("limux-pending-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let accel = gtk::Label::new(Some("Ctrl+Alt+P"));
    accel.add_css_class("limux-pending-dim");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("limux-pending-header");
    header.append(&title);
    header.append(&accel);

    let list = gtk::ListBox::new();
    list.add_css_class("limux-pending-list");
    list.set_selection_mode(gtk::SelectionMode::Browse);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(VISIBLE_ROWS * ROW_HEIGHT)
        .child(&list)
        .build();

    // The section headings already say what is waiting and what is unread;
    // the footer says how to move.
    let keys = gtk::Label::new(Some(
        "\u{2191}\u{2193} Enter \u{00B7} 1-9 jump \u{00B7} Del close",
    ));
    keys.add_css_class("limux-pending-dim");
    keys.set_xalign(0.0);
    keys.set_hexpand(true);
    let mark_all = gtk::Button::with_label("Mark all read");
    mark_all.add_css_class("limux-pending-mark-all");
    mark_all.set_focus_on_click(false);
    let clear_quiet = gtk::Button::with_label("Close quiet");
    clear_quiet.add_css_class("limux-pending-mark-all");
    clear_quiet.set_focus_on_click(false);
    clear_quiet.set_tooltip_text(Some("Close every quiet terminal from the lists"));
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    footer.add_css_class("limux-pending-footer");
    footer.append(&keys);
    footer.append(&clear_quiet);
    footer.append(&mark_all);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("limux-pending-panel");
    content.set_size_request(380, -1);
    content.append(&header);
    content.append(&scroll);
    content.append(&footer);

    let popover = gtk::Popover::new();
    popover.add_css_class("limux-pending-popover");
    popover.set_child(Some(&content));
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(false);
    popover.set_parent(&parent);
    if let Some(rect) = pointing_to {
        popover.set_pointing_to(Some(&rect));
    }

    {
        let state = state.clone();
        list.connect_row_activated(move |_, row| activate_row(&state, row));
    }
    {
        // 1-9 jump to the numbered entries.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let state = state.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let plain = !modifiers.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK,
            );
            if plain && matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                close_selected(&state);
                return glib::Propagation::Stop;
            }
            let digit = key.to_unicode().and_then(|c| c.to_digit(10));
            match digit {
                Some(n @ 1..=9) if plain => {
                    activate_number(&state, n as usize);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        content.add_controller(keys);
    }
    {
        let state = state.clone();
        mark_all.connect_clicked(move |_| mark_all_read(&state));
    }
    {
        let state = state.clone();
        clear_quiet.connect_clicked(move |_| close_quiet(&state));
    }
    {
        let previous_focus = parent
            .root()
            .and_then(|root| root.focus())
            .map(|widget| widget.downgrade());
        let state = state.clone();
        popover.connect_closed(move |popover| {
            if let Ok(mut s) = state.try_borrow_mut() {
                s.pending_open = None;
            }
            // The counter stayed for the open panel; hide it if nothing is
            // pending any more.
            {
                let state = state.clone();
                glib::idle_add_local_once(move || sync(&state));
            }
            // Hand the focus back unless a jump already moved it to a terminal.
            if crate::terminal::focus_is_within(popover.upcast_ref()) {
                let restored = previous_focus
                    .as_ref()
                    .and_then(|widget| widget.upgrade())
                    .is_some_and(|widget| widget.root().is_some() && widget.grab_focus());
                if !restored {
                    crate::terminal::unset_focus_within(popover.upcast_ref());
                }
            }
            let popover = popover.clone();
            // Unparent after GTK finished closing it.
            glib::idle_add_local_once(move || popover.unparent());
        });
    }

    let entries = pending_entries(state);
    let ages = fill_list(&list, &mark_all, &clear_quiet, &entries);
    state.borrow_mut().pending_open = Some(OpenPanel {
        popover: popover.clone(),
        list: list.clone(),
        mark_all,
        clear_quiet,
        entries,
        ages,
    });
    popover.popup();
    let weak_popover = popover.downgrade();
    let state_for_ages = state.clone();
    glib::timeout_add_seconds_local(1, move || {
        let open = weak_popover
            .upgrade()
            .is_some_and(|popover| popover.is_visible());
        if !open {
            return glib::ControlFlow::Break;
        }
        refresh_ages(&state_for_ages);
        glib::ControlFlow::Continue
    });
    if let Some(first) = list.row_at_index(0).filter(|row| row.is_activatable()) {
        list.select_row(Some(&first));
        first.grab_focus();
    } else if let Some(row) = first_activatable_row(&list) {
        list.select_row(Some(&row));
        row.grab_focus();
    }
}

fn first_activatable_row(list: &gtk::ListBox) -> Option<gtk::ListBoxRow> {
    let mut index = 0;
    while let Some(row) = list.row_at_index(index) {
        if row.is_activatable() {
            return Some(row);
        }
        index += 1;
    }
    None
}

fn fill_list(
    list: &gtk::ListBox,
    mark_all: &gtk::Button,
    clear_quiet: &gtk::Button,
    entries: &[PendingEntry],
) -> Vec<(gtk::Label, u64)> {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let count = |kind| entries.iter().filter(|entry| entry.kind == kind).count();
    mark_all.set_sensitive(count(PendingKind::Unread) > 0);
    clear_quiet.set_visible(count(PendingKind::Quiet) > 0);

    if entries.is_empty() {
        // Nothing to list: say what the signs mean instead.
        let legend = gtk::Box::new(gtk::Orientation::Vertical, 4);
        legend.add_css_class("limux-pending-legend");
        let heading = gtk::Label::new(Some("No agents need you."));
        heading.set_xalign(0.0);
        legend.append(&heading);
        for (glyph, class, text) in [
            (
                GLYPH_WAITING,
                "limux-pending-glyph-waiting",
                "Waiting for you: approval or a question",
            ),
            (
                GLYPH_UNREAD,
                "limux-pending-glyph",
                "Finished, not seen yet",
            ),
            (
                agent_activity::GLYPH_WORKING,
                "limux-pending-dim",
                "Working",
            ),
            (
                agent_activity::GLYPH_IDLE,
                "limux-pending-dim",
                "Quiet: did something, kept until you close it",
            ),
            (GLYPH_EXITED, "limux-pending-dim", "Exited"),
        ] {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let sign = gtk::Label::new(Some(glyph));
            sign.set_size_request(16, -1);
            sign.add_css_class(class);
            let label = gtk::Label::new(Some(text));
            label.set_xalign(0.0);
            line.append(&sign);
            line.append(&label);
            legend.append(&line);
        }
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_selectable(false);
        row.set_focusable(false);
        row.set_child(Some(&legend));
        list.append(&row);
        return Vec::new();
    }

    let now = agent_activity::now_ms();
    let mut ages = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let (row, age) = entry_row(entry, index, now);
        list.append(&row);
        ages.push((age, entry.at_ms));
    }
    // Section headings are list headers, not rows: the arrow keys go from
    // entry to entry across them, and Enter never lands on a heading.
    let kinds: Vec<PendingKind> = entries.iter().map(|entry| entry.kind).collect();
    list.set_header_func(move |row, before| {
        let kind_of = |row: &gtk::ListBoxRow| {
            row.widget_name()
                .strip_prefix("pending-")
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| kinds.get(index).copied())
        };
        let kind = kind_of(row);
        if kind.is_none() || kind == before.and_then(kind_of) {
            row.set_header(None::<&gtk::Widget>);
            return;
        }
        let kind = kind.expect("checked above");
        let in_section = kinds.iter().filter(|other| **other == kind).count();
        let label = gtk::Label::new(Some(&format!(
            "{} \u{00B7} {in_section}",
            match kind {
                PendingKind::Waiting => "WAITING FOR YOU",
                PendingKind::Unread => "FINISHED",
                PendingKind::Working => "WORKING",
                PendingKind::Quiet => "QUIET",
            }
        )));
        label.set_xalign(0.0);
        label.add_css_class("limux-pending-section");
        row.set_header(Some(&label));
    });
    ages
}

fn entry_row(entry: &PendingEntry, index: usize, now: u64) -> (gtk::ListBoxRow, gtk::Label) {
    if matches!(entry.kind, PendingKind::Working | PendingKind::Quiet) {
        return working_row(entry, index, now);
    }
    let glyph = gtk::Label::new(Some(match entry.kind {
        PendingKind::Waiting => GLYPH_WAITING,
        _ if entry.exited => GLYPH_EXITED,
        _ => GLYPH_UNREAD,
    }));
    glyph.add_css_class("limux-pending-glyph");
    if entry.kind == PendingKind::Waiting {
        glyph.add_css_class("limux-pending-glyph-waiting");
    } else if entry.exited {
        glyph.add_css_class("limux-pending-glyph-exited");
    }
    glyph.set_valign(gtk::Align::Start);

    let first_line = if entry.title.is_empty() {
        format!("<b>{}</b>", glib::markup_escape_text(&entry.workspace_name))
    } else {
        format!(
            "<b>{}</b> · {}",
            glib::markup_escape_text(&entry.workspace_name),
            glib::markup_escape_text(&entry.title)
        )
    };
    let line1 = gtk::Label::new(None);
    line1.set_markup(&first_line);
    line1.set_xalign(0.0);
    line1.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let line2 = gtk::Label::new(Some(&entry.message));
    line2.set_xalign(0.0);
    line2.set_ellipsize(gtk::pango::EllipsizeMode::End);
    line2.add_css_class("limux-pending-message");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);
    text.append(&line1);
    text.append(&line2);

    let age = gtk::Label::new(Some(&agent_activity::ago_label(now, entry.at_ms)));
    age.add_css_class("limux-pending-dim");
    age.set_xalign(1.0);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 3);
    right.set_valign(gtk::Align::Start);
    right.append(&age);
    if index < NUMBERED_ENTRIES {
        let key = gtk::Label::new(Some(&(index + 1).to_string()));
        key.add_css_class("limux-pending-key");
        key.set_halign(gtk::Align::End);
        right.append(&key);
    }

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.add_css_class("limux-pending-row-box");
    content.append(&glyph);
    content.append(&text);
    content.append(&right);

    let row = gtk::ListBoxRow::new();
    row.add_css_class("limux-pending-row");
    if let Some(color) = entry.group_color {
        row.add_css_class(sidebar_groups::group_color_class(color));
    }
    row.set_child(Some(&content));
    row.set_widget_name(&format!("pending-{index}"));
    let spoken = format!(
        "{}{}, {}",
        entry.workspace_name,
        if entry.title.is_empty() {
            String::new()
        } else {
            format!(", {}", entry.title)
        },
        entry.message
    );
    row.set_tooltip_text(Some(&spoken));
    row.update_property(&[gtk::accessible::Property::Label(&spoken)]);
    (row, age)
}

/// A working or quiet terminal: one compact line (sign, project, what it
/// does or last said, age).
fn working_row(entry: &PendingEntry, index: usize, now: u64) -> (gtk::ListBoxRow, gtk::Label) {
    // Working turns like everywhere else (the timer-drawn spinner shows in
    // the popover); quiet is a still ○.
    let spinner: gtk::Widget = if entry.kind == PendingKind::Quiet {
        gtk::Label::new(Some(agent_activity::GLYPH_IDLE)).upcast()
    } else {
        crate::working_spinner::new().upcast()
    };
    spinner.add_css_class("limux-pending-dim");
    spinner.set_size_request(16, -1);
    let name = if entry.title.is_empty() {
        format!("<b>{}</b>", glib::markup_escape_text(&entry.workspace_name))
    } else {
        format!(
            "<b>{}</b> \u{00B7} {}",
            glib::markup_escape_text(&entry.workspace_name),
            glib::markup_escape_text(&entry.title)
        )
    };
    let text = gtk::Label::new(None);
    text.set_markup(&format!(
        "{name}  <span alpha=\"70%\">{}</span>",
        glib::markup_escape_text(&entry.message)
    ));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let age = gtk::Label::new(Some(&agent_activity::ago_label(now, entry.at_ms)));
    age.add_css_class("limux-pending-dim");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.add_css_class("limux-pending-row-box");
    content.add_css_class("limux-pending-row-compact");
    content.append(&spinner);
    content.append(&text);
    content.append(&age);
    let row = gtk::ListBoxRow::new();
    row.add_css_class("limux-pending-row");
    if let Some(color) = entry.group_color {
        row.add_css_class(sidebar_groups::group_color_class(color));
    }
    row.set_child(Some(&content));
    row.set_widget_name(&format!("pending-{index}"));
    let spoken = format!(
        "{}, {}: {}",
        entry.workspace_name,
        if entry.kind == PendingKind::Quiet {
            "quiet"
        } else {
            "working"
        },
        entry.message
    );
    row.set_tooltip_text(Some(&spoken));
    row.update_property(&[gtk::accessible::Property::Label(&spoken)]);
    (row, age)
}

/// The number keys: jump to the `n`-th entry (1-based) when it has a number.
fn activate_number(state: &State, n: usize) {
    let row = {
        let s = state.borrow();
        let Some(open) = s.pending_open.as_ref() else {
            return;
        };
        if n == 0 || n > NUMBERED_ENTRIES || n > open.entries.len() {
            return;
        }
        let name = format!("pending-{}", n - 1);
        let mut found = None;
        let mut index = 0;
        while let Some(row) = open.list.row_at_index(index) {
            if row.widget_name() == name {
                found = Some(row);
                break;
            }
            index += 1;
        }
        found
    };
    if let Some(row) = row {
        activate_row(state, &row);
    }
}

fn activate_row(state: &State, row: &gtk::ListBoxRow) {
    let Some(index) = row
        .widget_name()
        .strip_prefix("pending-")
        .and_then(|index| index.parse::<usize>().ok())
    else {
        return;
    };
    let (popover, entry) = {
        let s = state.borrow();
        let Some(open) = s.pending_open.as_ref() else {
            return;
        };
        let Some(entry) = open.entries.get(index).cloned() else {
            return;
        };
        (open.popover.clone(), entry)
    };
    popover.popdown();
    let target = DesktopNotificationTarget {
        workspace_id: entry.workspace_id,
        pane_id: entry.pane_id,
        tab_id: entry.tab_id,
    };
    activate_desktop_notification_target(state, &target, None);
}

/// Delete: close the selected finished or quiet terminal from the lists.
fn close_selected(state: &State) {
    let (list, tab_id) = {
        let s = state.borrow();
        let Some(open) = s.pending_open.as_ref() else {
            return;
        };
        let Some(entry) = open
            .list
            .selected_row()
            .and_then(|row| {
                row.widget_name()
                    .strip_prefix("pending-")
                    .and_then(|index| index.parse::<usize>().ok())
            })
            .and_then(|index| open.entries.get(index))
        else {
            return;
        };
        if !matches!(entry.kind, PendingKind::Unread | PendingKind::Quiet) {
            return;
        }
        let Some(tab_id) = entry.tab_id.clone() else {
            return;
        };
        (open.list.clone(), tab_id)
    };
    agent_activity_ui::dismiss_terminal(state, &tab_id);
    if list.selected_row().is_none() {
        reselect(&list, Some(0));
    }
}

/// Select the row at `index` after the list was rebuilt (or the last one),
/// skipping section headings, and keep the keyboard in the list.
fn reselect(list: &gtk::ListBox, index: Option<i32>) {
    let Some(index) = index else {
        return;
    };
    let mut target = None;
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.is_activatable() {
            target = Some(row.clone());
            if i >= index {
                break;
            }
        }
        i += 1;
    }
    if let Some(row) = target {
        list.select_row(Some(&row));
        if list.root().is_some() {
            row.grab_focus();
        }
    }
}

/// Close every quiet terminal from the lists. Unread and active ones stay.
fn close_quiet(state: &State) {
    let quiet: Vec<String> = {
        let s = state.borrow();
        let unread: HashSet<String> = pending_entries_unread_tabs(&s);
        s.agent_activity
            .idle_tabs()
            .into_iter()
            .filter(|tab_id| !unread.contains(tab_id))
            .collect()
    };
    for tab_id in quiet {
        agent_activity_ui::dismiss_terminal(state, &tab_id);
    }
}

fn pending_entries_unread_tabs(s: &AppState) -> HashSet<String> {
    s.workspaces
        .iter()
        .flat_map(|workspace| workspace.split_container.panes())
        .filter_map(|pane_widget| pane::session_tabs_in_pane(&pane_widget))
        .flat_map(|tabs| tabs.tabs)
        .filter(|tab| tab.unread)
        .map(|tab| tab.tab_id)
        .collect()
}

/// Clear every unread mark. Waiting terminals stay: only the agent clears them.
fn mark_all_read(state: &State) {
    let workspaces: Vec<(String, Vec<gtk::Widget>)> = {
        let mut s = state.borrow_mut();
        s.tab_notes.clear();
        s.workspace_notes.clear();
        s.workspaces
            .iter_mut()
            .map(|workspace| {
                workspace.unread = false;
                workspace.manual_unread = false;
                (workspace.id.clone(), workspace.split_container.panes())
            })
            .collect()
    };
    for (_, panes) in &workspaces {
        pane::clear_all_tabs_unread_in_panes(panes);
    }
    for (id, _) in &workspaces {
        paint_workspace_unread(state, id);
    }
    sidebar_groups::sync_group_headers(state);
}

/// How many entries the open panel lists, `None` when it is closed.
#[cfg(test)]
pub(super) fn open_entry_count(state: &State) -> Option<usize> {
    let s = state.borrow();
    let open = s.pending_open.as_ref()?;
    open.popover.is_visible().then_some(open.entries.len())
}

/// The counter's visible text ("◆ 1 ● 2"), for tests.
#[cfg(test)]
pub(super) fn counter_text(state: &State) -> String {
    let Some(counter) = state.borrow().top_bar_pending_btn.clone() else {
        return String::new();
    };
    let mut parts = Vec::new();
    let mut child = counter.child().and_then(|content| content.first_child());
    while let Some(widget) = child {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if label.is_visible() {
                parts.push(label.label().to_string());
            }
        }
        child = widget.next_sibling();
    }
    parts.join(" ")
}

/// The open panel's popover, for snapshots.
#[cfg(test)]
pub(super) fn open_popover(state: &State) -> Option<gtk::Popover> {
    let s = state.borrow();
    s.pending_open.as_ref().map(|open| open.popover.clone())
}

/// The open panel's rows: (is an entry, carries a section heading), in
/// order. Headings are list headers, so every row is an entry.
#[cfg(test)]
pub(super) fn open_rows(state: &State) -> Vec<(bool, bool)> {
    let list = state
        .borrow()
        .pending_open
        .as_ref()
        .map(|open| open.list.clone())
        .expect("panel open");
    let mut rows = Vec::new();
    let mut index = 0;
    while let Some(row) = list.row_at_index(index) {
        rows.push((
            row.widget_name().starts_with("pending-") && row.is_focusable(),
            row.header().is_some(),
        ));
        index += 1;
    }
    rows
}

/// Activate the open panel's entry at `index` as a click or Enter would.
#[cfg(test)]
pub(super) fn activate_entry(state: &State, index: usize) {
    let list = state
        .borrow()
        .pending_open
        .as_ref()
        .map(|open| open.list.clone())
        .expect("panel open");
    let mut row_index = 0;
    while let Some(row) = list.row_at_index(row_index) {
        if row.widget_name() == format!("pending-{index}") {
            row.activate();
            return;
        }
        row_index += 1;
    }
    panic!("no entry {index}");
}

/// Press the open panel's "Mark all read".
#[cfg(test)]
pub(super) fn click_mark_all(state: &State) {
    let button = state
        .borrow()
        .pending_open
        .as_ref()
        .map(|open| open.mark_all.clone())
        .expect("panel open");
    button.emit_clicked();
}

pub(super) fn pending_panel_css() -> &'static str {
    r#"
/* ---------- Pending panel ---------- */
.limux-pending-counter {
    min-height: 0;
    padding: 1px 8px;
    margin: 0 2px;
    border-radius: 11px;
    font-size: 12px;
    font-weight: 700;
    background: alpha(@window_fg_color, 0.08);
}
.limux-pending-popover > contents {
    padding: 0;
}
.limux-pending-header {
    padding: 8px 12px;
    border-bottom: 1px solid alpha(@window_fg_color, 0.1);
}
.limux-pending-title {
    font-weight: 700;
}
.limux-pending-dim {
    font-size: 11px;
    color: alpha(@window_fg_color, 0.45);
}
.limux-pending-list {
    background: transparent;
}
.limux-pending-section {
    padding: 6px 12px 2px 12px;
    font-size: 10px;
    color: alpha(@window_fg_color, 0.45);
}
.limux-pending-row-box {
    padding: 6px 12px 6px 10px;
    min-height: 36px;
}
.limux-pending-glyph {
    min-width: 14px;
    font-size: 11px;
}
.limux-pending-message {
    font-size: 11px;
    color: alpha(@window_fg_color, 0.6);
}
.limux-pending-empty {
    padding: 22px 12px;
    color: alpha(@window_fg_color, 0.6);
}
.limux-pending-footer {
    padding: 6px 12px;
    border-top: 1px solid alpha(@window_fg_color, 0.1);
}
.limux-pending-mark-all {
    min-height: 0;
    padding: 2px 8px;
    font-size: 11px;
}
.limux-pending-key {
    font-family: monospace;
    font-size: 10px;
    padding: 0 4px;
    border-radius: 4px;
    background: alpha(@window_fg_color, 0.1);
    color: alpha(@window_fg_color, 0.7);
}
.limux-pending-row-box.limux-pending-row-compact {
    padding-top: 3px;
    padding-bottom: 3px;
    min-height: 22px;
    font-size: 12.5px;
}
.limux-pending-legend {
    padding: 12px 14px;
    font-size: 12.5px;
}
.limux-pending-glyph-exited {
    color: alpha(@window_fg_color, 0.8);
    font-weight: 700;
}
.limux-pending-count-working {
    font-weight: 400;
    color: alpha(@window_fg_color, 0.6);
}
"#
}
