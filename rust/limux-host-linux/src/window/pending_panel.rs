//! The pending panel: one counter in the toolbar (`◆ N` while anything waits,
//! else `● N`, hidden at zero) and a popover that lists every terminal waiting
//! for the user or unread, so finished work is one click away even with every
//! group collapsed or the sidebar hidden. `Ctrl+Alt+P` opens it from anywhere.
//!
//! It reads the same state as the project rows, pills and group headers (agent
//! activity and unread marks); it has no attention model of its own. Entries
//! clear by the existing rules only: unread after a project has been on
//! screen, waiting when the agent continues.

use super::*;
use crate::agent_activity::{self, PendingKind, GLYPH_UNREAD, GLYPH_WAITING};

/// Rows visible before the list scrolls.
const VISIBLE_ROWS: i32 = 8;
const ROW_HEIGHT: i32 = 50;

/// The popover that is open now, if any, and what it lists.
pub(super) struct OpenPanel {
    popover: gtk::Popover,
    list: gtk::ListBox,
    summary: gtk::Label,
    mark_all: gtk::Button,
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
    message: String,
}

/// The toolbar counter. Placed by `apply_top_bar_mode` next to the folder
/// button, or beside the sidebar toggle when both bars are hidden.
pub(super) fn build_counter() -> gtk::Button {
    let counter = gtk::Button::new();
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
                if !waiting && !tab.unread {
                    continue;
                }
                let note = s.tab_notes.get(&tab.tab_id);
                let (kind, at_ms, message) = if waiting {
                    let fallback = match activity.map(|activity| activity.phase) {
                        Some(agent_activity::AgentPhase::NeedsInput) => "Has a question",
                        _ => "Needs approval",
                    };
                    (
                        PendingKind::Waiting,
                        activity.map_or(0, |activity| activity.at_ms),
                        note.map_or(fallback.to_string(), |(text, _)| text.clone()),
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
                    title: tab.title,
                    message,
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
    let waiting = entries
        .iter()
        .filter(|entry| entry.kind == PendingKind::Waiting)
        .count();
    let unread = entries.len() - waiting;
    if entries.is_empty() {
        counter.set_visible(false);
    } else {
        let (glyph, count) = if waiting > 0 {
            (GLYPH_WAITING, waiting)
        } else {
            (GLYPH_UNREAD, unread)
        };
        counter.set_label(&format!("{glyph} {count}"));
        if waiting > 0 {
            counter.add_css_class("limux-pending-counter-waiting");
        } else {
            counter.remove_css_class("limux-pending-counter-waiting");
        }
        let tip = summary_text(waiting, unread);
        counter.set_tooltip_text(Some(&format!("{tip} (Ctrl+Alt+P)")));
        counter.update_property(&[gtk::accessible::Property::Label(&format!("Pending: {tip}"))]);
        counter.set_visible(true);
    }

    let open = state.borrow().pending_open.as_ref().map(|open| {
        (
            open.entries != entries,
            open.list.clone(),
            open.summary.clone(),
            open.mark_all.clone(),
        )
    });
    if let Some((changed, list, summary, mark_all)) = open {
        if changed {
            let ages = fill_list(&list, &summary, &mark_all, &entries);
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

fn summary_text(waiting: usize, unread: usize) -> String {
    match (waiting, unread) {
        (0, 0) => "Nothing pending".to_string(),
        (0, unread) => format!("{unread} unread"),
        (waiting, 0) => format!("{waiting} waiting for you"),
        (waiting, unread) => format!("{waiting} waiting for you, {unread} unread"),
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

    let title = gtk::Label::new(Some("Pending"));
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

    let summary = gtk::Label::new(None);
    summary.add_css_class("limux-pending-dim");
    summary.set_xalign(0.0);
    summary.set_hexpand(true);
    let mark_all = gtk::Button::with_label("Mark all read");
    mark_all.add_css_class("limux-pending-mark-all");
    mark_all.set_focus_on_click(false);
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.add_css_class("limux-pending-footer");
    footer.append(&summary);
    footer.append(&mark_all);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("limux-pending-panel");
    content.set_size_request(360, -1);
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
        let state = state.clone();
        mark_all.connect_clicked(move |_| mark_all_read(&state));
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
    let ages = fill_list(&list, &summary, &mark_all, &entries);
    state.borrow_mut().pending_open = Some(OpenPanel {
        popover: popover.clone(),
        list: list.clone(),
        summary,
        mark_all,
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
    summary: &gtk::Label,
    mark_all: &gtk::Button,
    entries: &[PendingEntry],
) -> Vec<(gtk::Label, u64)> {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let waiting = entries
        .iter()
        .filter(|entry| entry.kind == PendingKind::Waiting)
        .count();
    let unread = entries.len() - waiting;
    summary.set_label(&summary_text(waiting, unread));
    mark_all.set_sensitive(unread > 0);

    if entries.is_empty() {
        let empty = gtk::Label::new(Some("Nothing pending"));
        empty.add_css_class("limux-pending-empty");
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_selectable(false);
        row.set_child(Some(&empty));
        list.append(&row);
        return Vec::new();
    }

    let now = agent_activity::now_ms();
    let mut ages = Vec::new();
    let mut section = None;
    for (index, entry) in entries.iter().enumerate() {
        if section != Some(entry.kind) {
            section = Some(entry.kind);
            let label = gtk::Label::new(Some(match entry.kind {
                PendingKind::Waiting => "WAITING FOR YOU",
                PendingKind::Unread => "FINISHED",
            }));
            label.set_xalign(0.0);
            label.add_css_class("limux-pending-section");
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_child(Some(&label));
            list.append(&row);
        }
        let (row, age) = entry_row(entry, index, now);
        list.append(&row);
        ages.push((age, entry.at_ms));
    }
    ages
}

fn entry_row(entry: &PendingEntry, index: usize, now: u64) -> (gtk::ListBoxRow, gtk::Label) {
    let glyph = gtk::Label::new(Some(match entry.kind {
        PendingKind::Waiting => GLYPH_WAITING,
        PendingKind::Unread => GLYPH_UNREAD,
    }));
    glyph.add_css_class("limux-pending-glyph");
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
    age.set_valign(gtk::Align::Start);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.add_css_class("limux-pending-row-box");
    content.append(&glyph);
    content.append(&text);
    content.append(&age);

    let row = gtk::ListBoxRow::new();
    row.add_css_class("limux-pending-row");
    if let Some(color) = entry.group_color {
        row.add_css_class(&sidebar_groups::group_color_class(color));
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

/// The open panel's popover, for snapshots.
#[cfg(test)]
pub(super) fn open_popover(state: &State) -> Option<gtk::Popover> {
    let s = state.borrow();
    s.pending_open.as_ref().map(|open| open.popover.clone())
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
.limux-pending-counter-waiting {
    background: alpha(#f59e0b, 0.18);
    box-shadow: inset 0 0 0 1px alpha(#f59e0b, 0.5);
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
"#
}
