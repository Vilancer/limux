//! Sidebar rows for workspace groups: the header widgets, their menu and drag
//! and drop, and `sync_sidebar`, which mirrors `AppState::sidebar` in the GTK
//! list. The order and its rules live in `workspace_group`.

use super::*;
use crate::agent_activity::{ActivityCounts, GLYPH_UNREAD, GLYPH_WAITING};
use crate::control_bridge::GroupRequest;
use crate::workspace_group::{
    ActivePreviews, Activity, GroupError, SidebarRow, WorkspaceGroup, ACTIVE_PREVIEW_CAP,
};

/// Drag payload prefix for a group header. Tab payloads are `pane:tab` with
/// a numeric pane, so this never parses as one.
pub(super) const GROUP_DRAG_PREFIX: &str = "limux-group:";

const MEMBER_CSS_CLASS: &str = "limux-ws-group-member";
/// Prefix of the class that colours a group's dot in the workspace menu.
const DOT_CSS_PREFIX: &str = "limux-ws-group-dot-";

pub(super) struct GroupHeader {
    row: gtk::ListBoxRow,
    chevron: gtk::Label,
    pill: gtk::Box,
    name_label: gtk::Label,
    count_label: gtk::Label,
    /// The number inside the working button, next to its spinner.
    working_count: gtk::Label,
    /// "↻ N": working terminals in the group, shown even when collapsed; a
    /// click goes to the next one.
    working_label: gtk::Button,
    /// "◆ N": waiting terminals; a click goes to the next one.
    waiting_button: gtk::Button,
    /// "● N": unread projects, when none waits; a click goes to the next one.
    unread_button: gtk::Button,
    /// Below a collapsed group: "N more", the active projects past the cap.
    more_row: gtk::ListBoxRow,
    more_button: gtk::Button,
}

pub(super) fn group_color_class(color: WorkspaceColor) -> String {
    format!("limux-ws-group-{}", color.as_str())
}

/// CSS for group headers and the stripe on their members, from the palette.
pub(super) fn workspace_group_css() -> String {
    let mut css = String::from(
        r#"
/* ---------- Workspace groups ---------- */
.limux-ws-group-header {
    padding: 4px 8px;
    margin: 4px 6px 0 6px;
    border-radius: 8px;
    background: #2e2e2e;
}
.limux-sidebar-list row:hover .limux-ws-group-header {
    background: #383838;
}
.limux-ws-group-chevron {
    color: alpha(white, 0.7);
    font-size: 15px;
    min-width: 20px;
}
.limux-ws-group-pill {
    border-radius: 6px;
    padding: 1px 8px;
}
.limux-ws-group-name {
    color: white;
    font-size: 12px;
    font-weight: 600;
}
.limux-ws-group-count {
    color: alpha(white, 0.6);
    font-size: 11px;
    margin-left: 6px;
}
.limux-sidebar-list row.limux-ws-group-member {
    margin-left: 8px;
}
.limux-drop-above .limux-ws-group-header {
    border-radius: 0;
    box-shadow: 0 -2px 0 0 @accent_bg_color;
}
.limux-drop-below .limux-ws-group-header {
    border-radius: 0;
    box-shadow: 0 2px 0 0 @accent_bg_color;
}
"#,
    );
    for color in WorkspaceColor::ALL {
        let class = group_color_class(color);
        let hex = color.css_hex();
        // Loaded after the row rules that clear `box-shadow`, at equal
        // specificity, so the stripe wins in every row state.
        css.push_str(&format!(
            ".limux-ws-group-pill.{class} {{\n    background: {hex};\n}}\n\
             .limux-sidebar-list row.{MEMBER_CSS_CLASS}.{class} {{\n    box-shadow: inset 3px 0 0 0 {hex};\n}}\n\
             .{DOT_CSS_PREFIX}{name} {{\n    color: {hex};\n}}\n\
             .limux-pending-row.{class} {{\n    box-shadow: inset 3px 0 0 0 {hex};\n}}\n",
            name = color.as_str(),
        ));
    }
    css
}

fn build_group_header(state: &State, group_id: &str) -> GroupHeader {
    let chevron = gtk::Label::new(None);
    chevron.add_css_class("limux-ws-group-chevron");

    let name_label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    name_label.add_css_class("limux-ws-group-name");
    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pill.add_css_class("limux-ws-group-pill");
    pill.append(&name_label);

    let count_label = gtk::Label::new(None);
    count_label.add_css_class("limux-ws-group-count");

    let working_label = gtk::Button::new();
    let working_count = gtk::Label::new(None);
    let working_content = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    working_content.append(&agent_activity_ui::working_spinner());
    working_content.append(&working_count);
    working_label.set_child(Some(&working_content));
    working_label.add_css_class("flat");
    working_label.add_css_class("limux-ws-group-working");
    working_label.set_focus_on_click(false);
    working_label.set_valign(gtk::Align::Center);
    working_label.set_visible(false);
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        working_label.connect_clicked(move |_| cycle_working_terminal(&state, &group_id));
    }

    let waiting_button = gtk::Button::new();
    waiting_button.add_css_class("flat");
    waiting_button.add_css_class("limux-ws-group-waiting");
    waiting_button.add_css_class("limux-notify-dot");
    waiting_button.add_css_class("limux-ws-group-needs-you");
    waiting_button.set_focus_on_click(false);
    waiting_button.set_valign(gtk::Align::Center);
    waiting_button.set_visible(false);
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        waiting_button.connect_clicked(move |_| {
            cycle_attention_terminal(&state, true, Some(&group_id));
        });
    }

    let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header_box.add_css_class("limux-ws-group-header");
    header_box.append(&chevron);
    header_box.append(&pill);
    header_box.append(&count_label);
    let unread_button = gtk::Button::new();
    unread_button.add_css_class("flat");
    unread_button.add_css_class("limux-ws-group-waiting");
    unread_button.add_css_class("limux-notify-dot");
    unread_button.set_focus_on_click(false);
    unread_button.set_valign(gtk::Align::Center);
    unread_button.set_visible(false);
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        unread_button.connect_clicked(move |_| {
            cycle_unread_in_group(&state, &group_id);
        });
    }

    // Counts sit on the right, loudest first: waiting, unread, working.
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header_box.append(&spacer);
    header_box.append(&waiting_button);
    header_box.append(&unread_button);
    header_box.append(&working_label);

    let more_button = gtk::Button::new();
    more_button.add_css_class("flat");
    more_button.add_css_class("limux-ws-group-more");
    more_button.set_halign(gtk::Align::Start);
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        more_button.connect_clicked(move |_| show_next_hidden_active(&state, &group_id));
    }
    let more_row = gtk::ListBoxRow::new();
    more_row.set_selectable(false);
    more_row.set_activatable(false);
    more_row.set_child(Some(&more_button));
    more_row.set_visible(false);

    let row = gtk::ListBoxRow::new();
    row.add_css_class("limux-ws-group-row");
    // Headers never become the active workspace; activating one (a click or
    // Enter) collapses or expands the group instead.
    row.set_selectable(false);
    row.set_child(Some(&header_box));
    install_group_header_interactions(state, group_id, &row);

    GroupHeader {
        row,
        chevron,
        pill,
        name_label,
        count_label,
        working_label,
        working_count,
        waiting_button,
        unread_button,
        more_row,
        more_button,
    }
}

/// Show a group's working and waiting counts on its header, and how many
/// waiting projects the collapsed group could not list.
fn update_group_activity(
    header: &GroupHeader,
    group: &WorkspaceGroup,
    counts: ActivityCounts,
    unread_projects: usize,
    hidden_active: usize,
    hidden_working_only: bool,
) {
    header.working_count.set_label(&counts.working.to_string());
    header.working_label.set_visible(counts.working > 0);
    let working_tip = format!(
        "{}: {} agent{} working. Go to the next one",
        group.name,
        counts.working,
        if counts.working == 1 { "" } else { "s" }
    );
    header.working_label.set_tooltip_text(Some(&working_tip));
    header
        .working_label
        .update_property(&[gtk::accessible::Property::Label(&working_tip)]);
    header
        .waiting_button
        .set_label(&format!("{GLYPH_WAITING} {}", counts.waiting));
    header.waiting_button.set_visible(counts.waiting > 0);
    let waiting_tip = format!(
        "{}: {} need{} you. Go to the next one",
        group.name,
        counts.waiting,
        if counts.waiting == 1 { "s" } else { "" }
    );
    header.waiting_button.set_tooltip_text(Some(&waiting_tip));
    header
        .waiting_button
        .update_property(&[gtk::accessible::Property::Label(&waiting_tip)]);

    header
        .unread_button
        .set_label(&format!("{GLYPH_UNREAD} {unread_projects}"));
    header.unread_button.set_visible(unread_projects > 0);
    let unread_tip = format!(
        "{}: {unread_projects} unread project{}. Go to the next one",
        group.name,
        if unread_projects == 1 { "" } else { "s" }
    );
    header.unread_button.set_tooltip_text(Some(&unread_tip));
    header
        .unread_button
        .update_property(&[gtk::accessible::Property::Label(&unread_tip)]);

    header.more_button.set_label(&format!(
        "+{hidden_active} {} \u{203A}",
        if hidden_working_only {
            "working"
        } else {
            "more"
        }
    ));
    let more_tip = format!(
        "{hidden_active} more active project{} in {}. Go to the next one",
        if hidden_active == 1 { "" } else { "s" },
        group.name
    );
    header.more_button.set_tooltip_text(Some(&more_tip));
    header
        .more_button
        .update_property(&[gtk::accessible::Property::Label(&more_tip)]);

    let mut summary = header
        .row
        .tooltip_text()
        .map(|text| text.to_string())
        .unwrap_or_default();
    if let Some(activity) = crate::agent_activity::counts_summary(counts) {
        summary = format!("{summary}; {activity}");
    }
    header
        .row
        .update_property(&[gtk::accessible::Property::Label(&summary)]);
}

/// Activity counts per workspace, the waiting projects collapsed groups keep
/// visible, and the counts per group.
struct GroupActivity {
    previews: ActivePreviews,
    by_group: HashMap<String, ActivityCounts>,
    /// Group id → members with unread marks.
    unread_by_group: HashMap<String, usize>,
    /// Group id → whether every member the previews left out is only working.
    more_working_only: HashMap<String, bool>,
}

fn group_activity(s: &AppState) -> GroupActivity {
    let by_workspace: HashMap<String, ActivityCounts> = s
        .workspaces
        .iter()
        .map(|workspace| {
            (
                workspace.id.clone(),
                agent_activity_ui::workspace_activity_counts(s, &workspace.id),
            )
        })
        .collect();
    let active = s.active_workspace().map(|workspace| workspace.id.as_str());
    let unread: HashSet<&str> = s
        .workspaces
        .iter()
        .filter(|workspace| {
            agent_activity_ui::has_finished_unread(
                s,
                &workspace.id,
                workspace.unread || workspace.manual_unread,
            )
        })
        .map(|workspace| workspace.id.as_str())
        .collect();
    let previews = s.sidebar.active_previews(
        active,
        |id| {
            let counts = by_workspace.get(id)?;
            if counts.waiting > 0 {
                Some(Activity::Waiting)
            } else if unread.contains(id) {
                Some(Activity::Unread)
            } else if counts.working > 0 {
                Some(Activity::Working)
            } else if counts.quiet > 0 {
                Some(Activity::Quiet)
            } else {
                None
            }
        },
        ACTIVE_PREVIEW_CAP,
    );
    let by_group = s
        .sidebar
        .groups()
        .map(|group| {
            let mut counts = ActivityCounts::default();
            for member in group.members() {
                if let Some(member) = by_workspace.get(member) {
                    counts.add(*member);
                }
            }
            (group.id.clone(), counts)
        })
        .collect();
    let unread_by_group = s
        .sidebar
        .groups()
        .map(|group| {
            let count = group
                .members()
                .iter()
                .filter(|member| unread.contains(member.as_str()))
                .count();
            (group.id.clone(), count)
        })
        .collect();
    // "+2 working ›" when everything left out is only working.
    let more_working_only = previews
        .more
        .iter()
        .map(|(group, hidden)| {
            let working_only = hidden.iter().all(|id| {
                !unread.contains(id.as_str())
                    && by_workspace
                        .get(id)
                        .is_some_and(|counts| counts.waiting == 0 && counts.working > 0)
            });
            (group.clone(), working_only)
        })
        .collect();
    GroupActivity {
        previews,
        by_group,
        unread_by_group,
        more_working_only,
    }
}

/// The "N more" row: go to the first active project the collapsed group left
/// out (waiting ones first). Once it is selected it shows as the selected row, so the
/// next click reaches the next one.
fn show_next_hidden_active(state: &State, group_id: &str) {
    let first_hidden = {
        let s = state.borrow();
        group_activity(&s)
            .previews
            .more
            .get(group_id)
            .and_then(|hidden| hidden.first().cloned())
    };
    let Some(workspace_id) = first_hidden else {
        return;
    };
    // The terminal that put it there, ranked as the previews are: waiting,
    // unread, working, then quiet.
    let target_tab = {
        let s = state.borrow();
        let panes = s
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .map(|workspace| workspace.split_container.panes())
            .unwrap_or_default();
        panes
            .iter()
            .filter_map(pane::session_tabs_in_pane)
            .flat_map(|pane_tabs| {
                let pane_id = pane_tabs.pane_id;
                pane_tabs.tabs.into_iter().map(move |tab| (pane_id, tab))
            })
            .filter_map(|(pane_id, tab)| {
                let phase = s.agent_activity.phase(&tab.tab_id);
                let rank = match phase {
                    Some(phase) if phase.is_waiting() => 0,
                    _ if tab.unread => 1,
                    Some(crate::agent_activity::AgentPhase::Working) => 2,
                    Some(_) => 3,
                    None => return None,
                };
                Some((rank, pane_id, tab.tab_id))
            })
            .min_by_key(|(rank, _, _)| *rank)
    };
    let target = DesktopNotificationTarget {
        workspace_id,
        pane_id: target_tab.as_ref().map(|(_, pane_id, _)| *pane_id),
        tab_id: target_tab.map(|(_, _, tab_id)| tab_id),
    };
    activate_desktop_notification_target(state, &target, None);
}

/// Click a group header's `● N` count, as the user would.
#[cfg(test)]
pub(super) fn click_unread_count(state: &State, group_id: &str) {
    let button = state.borrow().group_headers[group_id].unread_button.clone();
    assert!(button.is_visible(), "the unread count is shown");
    button.emit_clicked();
}

/// Click a collapsed group's `+N more` row.
#[cfg(test)]
pub(super) fn click_more(state: &State, group_id: &str) {
    let button = state.borrow().group_headers[group_id].more_button.clone();
    button.emit_clicked();
}

fn update_group_header(header: &GroupHeader, group: &WorkspaceGroup) {
    header.chevron.set_label(if group.collapsed {
        "\u{25B8}"
    } else {
        "\u{25BE}"
    });
    header.name_label.set_label(&group.name);
    // The counts beside it must not squeeze a short name into "…".
    let name_chars = i32::try_from(group.name.chars().count()).unwrap_or(i32::MAX);
    header.name_label.set_width_chars(name_chars.min(8));
    for color in WorkspaceColor::ALL {
        header.pill.remove_css_class(&group_color_class(color));
    }
    header.pill.add_css_class(&group_color_class(group.color));
    let count = group.members().len();
    header.count_label.set_label(&count.to_string());
    // The project count is in the tooltip; only an empty group says "0".
    header.count_label.set_visible(count == 0);
    let state = if group.collapsed {
        "collapsed"
    } else {
        "expanded"
    };
    let summary = format!(
        "Group {}, {count} workspace{}, {state}",
        group.name,
        if count == 1 { "" } else { "s" }
    );
    header.row.set_tooltip_text(Some(&summary));
    header
        .row
        .update_property(&[gtk::accessible::Property::Label(&summary)]);
}

fn set_member_style(row: &gtk::ListBoxRow, color: Option<WorkspaceColor>) {
    for candidate in WorkspaceColor::ALL {
        row.remove_css_class(&group_color_class(candidate));
    }
    match color {
        Some(color) => {
            row.add_css_class(MEMBER_CSS_CLASS);
            row.add_css_class(&group_color_class(color));
        }
        None => row.remove_css_class(MEMBER_CSS_CLASS),
    }
}

/// Mirror `AppState::sidebar` in the UI: put `workspaces`, the sidebar rows
/// and the top-bar pills in its order, build or drop group headers, and
/// refresh group styling, favorites and collapsed rows.
pub(super) fn sync_sidebar(state: &State) {
    let (sidebar_list, indicator_box, rows, indicators, row_to_select) = {
        let mut guard = state.borrow_mut();
        let s = &mut *guard;
        let order = s.sidebar.workspace_order();
        let active_id = s.active_workspace().map(|workspace| workspace.id.clone());
        s.workspaces.sort_by_key(|workspace| {
            order
                .iter()
                .position(|id| *id == workspace.id)
                .unwrap_or(usize::MAX)
        });
        if let Some(index) = active_id.as_ref().and_then(|id| {
            s.workspaces
                .iter()
                .position(|workspace| &workspace.id == id)
        }) {
            s.active_idx = index;
        }
        sync_indicator_active_state(s);

        let activity = group_activity(s);
        let sidebar = &s.sidebar;
        s.group_headers.retain(|id, _| sidebar.group(id).is_some());
        for group in sidebar.groups() {
            let header = s
                .group_headers
                .entry(group.id.clone())
                .or_insert_with(|| build_group_header(state, &group.id));
            update_group_header(header, group);
            update_group_activity(
                header,
                group,
                activity
                    .by_group
                    .get(&group.id)
                    .copied()
                    .unwrap_or_default(),
                activity
                    .unread_by_group
                    .get(&group.id)
                    .copied()
                    .unwrap_or_default(),
                activity.previews.more.get(&group.id).map_or(0, Vec::len),
                activity
                    .more_working_only
                    .get(&group.id)
                    .copied()
                    .unwrap_or(false),
            );
        }

        let mut rows = Vec::new();
        // A group's "N more" row goes after its last member.
        let mut pending_more: Option<(gtk::ListBoxRow, bool)> = None;
        for row in sidebar.rows() {
            if matches!(
                row,
                SidebarRow::GroupHeader(_) | SidebarRow::Workspace { group: None, .. }
            ) {
                rows.extend(pending_more.take());
            }
            let widget = match row {
                SidebarRow::GroupHeader(group) => {
                    let header = &s.group_headers[&group.id];
                    pending_more = Some((
                        header.more_row.clone(),
                        group.collapsed && activity.previews.more.contains_key(&group.id),
                    ));
                    header.row.clone()
                }
                SidebarRow::Workspace { id, group } => {
                    let Some(workspace) = s.workspaces.iter().find(|w| w.id == id) else {
                        continue;
                    };
                    set_member_style(&workspace.sidebar_row, group.map(|group| group.color));
                    set_workspace_favorite_visual(workspace, sidebar.is_favorite(id));
                    workspace.sidebar_row.clone()
                }
            };
            rows.push((
                widget,
                row.visible(active_id.as_deref(), &activity.previews),
            ));
        }
        rows.extend(pending_more.take());
        let indicators: Vec<gtk::Button> = s
            .workspaces
            .iter()
            .map(|workspace| workspace.indicator_button.clone())
            .collect();
        let row_to_select = s
            .active_workspace()
            .map(|workspace| workspace.sidebar_row.clone());
        (
            s.sidebar_list.clone(),
            s.indicator_box.clone(),
            rows,
            indicators,
            row_to_select,
        )
    };

    // Mutate GTK container children after dropping `state.borrow_mut()` so
    // synchronous `drag_source.connect_drag_end` callbacks do not panic.
    // Hiding a row can move the focus out of an inline-rename entry, whose
    // commit borrows the state too.
    set_rows_visible(&rows);
    let rows: Vec<gtk::ListBoxRow> = rows.into_iter().map(|(row, _)| row).collect();
    replace_children(
        sidebar_list.upcast_ref(),
        &rows,
        |child| sidebar_list.remove(child),
        |child| sidebar_list.append(child),
    );
    replace_children(
        indicator_box.upcast_ref(),
        &indicators,
        |child| indicator_box.remove(child),
        |child| indicator_box.append(child),
    );

    if let Some(row) = row_to_select {
        sidebar_list.select_row(Some(&row));
    }
    // Collapsing or expanding a group changes which members are one-line
    // previews.
    sync_row_visibility(state);
}

/// Re-add `children` in order, unless the container already holds exactly them.
fn replace_children<W: IsA<gtk::Widget>>(
    container: &gtk::Widget,
    children: &[W],
    remove: impl Fn(&gtk::Widget),
    append: impl Fn(&W),
) {
    let mut current = Vec::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        current.push(widget);
    }
    let unchanged = current.len() == children.len()
        && current
            .iter()
            .zip(children)
            .all(|(current, wanted)| current == wanted.upcast_ref());
    if unchanged {
        return;
    }
    for widget in &current {
        remove(widget);
    }
    for widget in children {
        append(widget);
    }
}

/// Show or hide member rows of collapsed groups for the active workspace and
/// the waiting ones, and refresh the groups' activity counts.
pub(super) fn sync_row_visibility(state: &State) {
    let mut previews_now = HashSet::new();
    let rows: Vec<(gtk::ListBoxRow, bool)> = {
        let s = state.borrow();
        let activity = group_activity(&s);
        previews_now.clone_from(&activity.previews.shown);
        let active = s.active_workspace().map(|workspace| workspace.id.as_str());
        let mut rows = Vec::new();
        for row in s.sidebar.rows() {
            match row {
                SidebarRow::Workspace { id, .. } => {
                    if let Some(workspace) = s.workspaces.iter().find(|w| w.id == id) {
                        rows.push((
                            workspace.sidebar_row.clone(),
                            row.visible(active, &activity.previews),
                        ));
                    }
                }
                SidebarRow::GroupHeader(group) => {
                    if let Some(header) = s.group_headers.get(&group.id) {
                        let hidden = activity.previews.more.get(&group.id).map_or(0, Vec::len);
                        update_group_activity(
                            header,
                            group,
                            activity
                                .by_group
                                .get(&group.id)
                                .copied()
                                .unwrap_or_default(),
                            activity
                                .unread_by_group
                                .get(&group.id)
                                .copied()
                                .unwrap_or_default(),
                            hidden,
                            activity
                                .more_working_only
                                .get(&group.id)
                                .copied()
                                .unwrap_or(false),
                        );
                        rows.push((header.more_row.clone(), group.collapsed && hidden > 0));
                    }
                }
            }
        }
        rows
    };
    set_rows_visible(&rows);
    // Members shown under a collapsed group become one-line previews.
    let changed: Vec<String> = {
        let mut s = state.borrow_mut();
        let changed = s
            .preview_rows
            .symmetric_difference(&previews_now)
            .cloned()
            .collect();
        s.preview_rows = previews_now;
        changed
    };
    for id in changed {
        agent_activity_ui::refresh_session_rows(state, &id, true);
    }
    pending_panel::sync(state);
}

/// Refresh the groups' working, waiting and unread counts on their headers,
/// and the pending counter.
pub(super) fn sync_group_headers(state: &State) {
    paint_group_headers(state);
    pending_panel::sync(state);
}

fn paint_group_headers(state: &State) {
    let s = state.borrow();
    let activity = group_activity(&s);
    for group in s.sidebar.groups() {
        if let Some(header) = s.group_headers.get(&group.id) {
            update_group_activity(
                header,
                group,
                activity
                    .by_group
                    .get(&group.id)
                    .copied()
                    .unwrap_or_default(),
                activity
                    .unread_by_group
                    .get(&group.id)
                    .copied()
                    .unwrap_or_default(),
                activity.previews.more.get(&group.id).map_or(0, Vec::len),
                activity
                    .more_working_only
                    .get(&group.id)
                    .copied()
                    .unwrap_or(false),
            );
        }
    }
}

/// Call without holding the state: hiding the row that holds the focus runs
/// focus-leave handlers synchronously.
fn set_rows_visible(rows: &[(gtk::ListBoxRow, bool)]) {
    for (row, visible) in rows {
        row.set_visible(*visible);
    }
}

/// Rebuild the sidebar from a loaded session, after its workspaces were added.
pub(super) fn restore_groups(state: &State, session: &AppSessionState) {
    {
        let mut guard = state.borrow_mut();
        let s = &mut *guard;
        // Workspaces added before the session's own (none at startup) keep
        // their place ahead of it.
        let earlier = s.workspaces.len().saturating_sub(session.workspaces.len());
        let saved: Vec<(String, bool, Option<&str>)> = s.workspaces[..earlier]
            .iter()
            .map(|workspace| {
                (
                    workspace.id.clone(),
                    s.sidebar.is_favorite(&workspace.id),
                    None,
                )
            })
            .chain(s.workspaces[earlier..].iter().zip(&session.workspaces).map(
                |(workspace, saved)| {
                    (
                        workspace.id.clone(),
                        saved.favorite,
                        saved.group_id.as_deref(),
                    )
                },
            ))
            .collect();
        s.sidebar = SidebarLayout::from_saved(saved, &session.groups);
    }
    sync_sidebar(state);
}

/// Apply a change to the sidebar layout; on success mirror it and save.
fn update_layout<T>(
    state: &State,
    change: impl FnOnce(&mut SidebarLayout) -> Result<T, GroupError>,
) -> Result<T, GroupError> {
    let result = change(&mut state.borrow_mut().sidebar);
    if result.is_ok() {
        sync_sidebar(state);
        request_session_save(state);
    }
    result
}

pub(super) fn create_group(
    state: &State,
    name: Option<&str>,
    color: Option<WorkspaceColor>,
    members: &[String],
) -> Result<String, GroupError> {
    update_layout(state, |sidebar| sidebar.create_group(name, color, members))
}

pub(super) fn rename_group(state: &State, group_id: &str, name: &str) -> Result<(), GroupError> {
    update_layout(state, |sidebar| sidebar.rename_group(group_id, name))
}

pub(super) fn set_group_color(
    state: &State,
    group_id: &str,
    color: WorkspaceColor,
) -> Result<(), GroupError> {
    update_layout(state, |sidebar| sidebar.set_group_color(group_id, color))
}

pub(super) fn set_group_collapsed(
    state: &State,
    group_id: &str,
    collapsed: bool,
) -> Result<(), GroupError> {
    update_layout(state, |sidebar| {
        sidebar.set_group_collapsed(group_id, collapsed)
    })
}

pub(super) fn delete_group(state: &State, group_id: &str) -> Result<(), GroupError> {
    update_layout(state, |sidebar| sidebar.delete_group(group_id))
}

pub(super) fn set_workspace_group(
    state: &State,
    workspace_id: &str,
    group_id: Option<&str>,
) -> Result<(), GroupError> {
    update_layout(state, |sidebar| {
        sidebar.set_workspace_group(workspace_id, group_id)
    })
}

pub(super) fn move_group(
    state: &State,
    group_id: &str,
    target: &RowRef,
    placement: Placement,
) -> bool {
    update_layout(state, |sidebar| {
        if sidebar.move_group(group_id, target, placement) {
            Ok(())
        } else {
            Err(GroupError::UnknownGroup)
        }
    })
    .is_ok()
}

fn move_workspace_to_header(
    state: &State,
    workspace_id: &str,
    group_id: &str,
    placement: Placement,
) -> bool {
    update_layout(state, |sidebar| {
        if sidebar.move_workspace(
            workspace_id,
            &RowRef::Group(group_id.to_string()),
            placement,
        ) {
            Ok(())
        } else {
            Err(GroupError::UnknownWorkspace)
        }
    })
    .is_ok()
}

fn header_group_id(state: &State, row: &gtk::ListBoxRow) -> Option<String> {
    state
        .borrow()
        .group_headers
        .iter()
        .find(|(_, header)| &header.row == row)
        .map(|(id, _)| id.clone())
}

/// A click or Enter on a group header collapses or expands it.
pub(super) fn activate_header_row(state: &State, row: &gtk::ListBoxRow) {
    let Some(group_id) = header_group_id(state, row) else {
        return;
    };
    let collapsed = state
        .borrow()
        .sidebar
        .group(&group_id)
        .is_some_and(|group| group.collapsed);
    let _ = set_group_collapsed(state, &group_id, !collapsed);
}

fn begin_group_inline_rename(state: &State, group_id: &str) {
    let (label, current_name) = {
        let s = state.borrow();
        let (Some(header), Some(group)) =
            (s.group_headers.get(group_id), s.sidebar.group(group_id))
        else {
            return;
        };
        (header.name_label.clone(), group.name.clone())
    };
    let group_id = group_id.to_string();
    begin_inline_rename(state, &label, &current_name, move |state, name| {
        let _ = rename_group(state, &group_id, &name);
    });
}

/// Open a new workspace in the group's last member's folder, inside the group.
fn add_workspace_to_group(state: &State, group_id: &str) {
    let folder = {
        let s = state.borrow();
        let Some(group) = s.sidebar.group(group_id) else {
            return;
        };
        group
            .members()
            .last()
            .and_then(|id| s.workspaces.iter().find(|w| &w.id == id))
            .and_then(|workspace| {
                workspace
                    .folder_path
                    .clone()
                    .or_else(|| workspace.cwd.borrow().clone())
            })
    };
    let folder = folder
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let folder_path = folder.to_string_lossy().to_string();
    let name = folder
        .file_name()
        .map(|segment| segment.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder_path.clone());
    let context = GuiHookContext {
        workspace_name: Some(name.clone()),
        folder: Some(folder_path.clone()),
        group_id: Some(group_id.to_string()),
        ..GuiHookContext::default()
    };
    if !super::gui_hooks::run_gui_hook(state, GuiHookEvent::OnWorkspaceCreate, &context) {
        return;
    }
    create_workspace_with_folder(state, &name, &folder_path);
    let new_id = state
        .borrow()
        .active_workspace()
        .map(|workspace| workspace.id.clone());
    if let Some(new_id) = new_id {
        let _ = set_workspace_group(state, &new_id, Some(group_id));
        super::gui_hooks::notify_workspace_created(state, context);
    }
}

fn menu_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("flat");
    button
}

/// Connect a menu button: close the menu, then run `action`.
fn on_menu_click(button: &gtk::Button, popover: &gtk::Popover, action: impl Fn() + 'static) {
    let popover = popover.downgrade();
    button.connect_clicked(move |_| {
        if let Some(popover) = popover.upgrade() {
            popover.popdown();
        }
        action();
    });
}

fn show_group_context_menu(state: &State, group_id: &str, row: &gtk::ListBoxRow) {
    let Some((collapsed, color, member_count)) = state
        .borrow()
        .sidebar
        .group(group_id)
        .map(|group| (group.collapsed, group.color, group.members().len()))
    else {
        return;
    };
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu_box.set_margin_top(4);
    menu_box.set_margin_bottom(4);
    menu_box.set_margin_start(4);
    menu_box.set_margin_end(4);

    let rename_btn = menu_button("Rename");
    let new_workspace_btn = menu_button("New workspace in group");
    let collapse_btn = menu_button(if collapsed { "Expand" } else { "Collapse" });
    let (swatch_row, swatches) = build_color_swatches(Some(color), false);
    let delete_btn = menu_button("Delete group");
    delete_btn.add_css_class("destructive-action");
    if member_count > 0 {
        delete_btn.set_sensitive(false);
        delete_btn.set_tooltip_text(Some(&GroupError::NotEmpty(member_count).to_string()));
    }

    menu_box.append(&rename_btn);
    menu_box.append(&new_workspace_btn);
    menu_box.append(&collapse_btn);
    menu_box.append(&swatch_row);
    menu_box.append(&delete_btn);

    let popover = sidebar_menu_popover(state, row, &menu_box);
    let group_id = group_id.to_string();
    {
        let state = state.clone();
        let group_id = group_id.clone();
        on_menu_click(&rename_btn, &popover, move || {
            begin_group_inline_rename(&state, &group_id);
        });
    }
    {
        let state = state.clone();
        let group_id = group_id.clone();
        on_menu_click(&new_workspace_btn, &popover, move || {
            add_workspace_to_group(&state, &group_id);
        });
    }
    {
        let state = state.clone();
        let group_id = group_id.clone();
        on_menu_click(&collapse_btn, &popover, move || {
            let _ = set_group_collapsed(&state, &group_id, !collapsed);
        });
    }
    for (color, button) in swatches {
        let state = state.clone();
        let group_id = group_id.clone();
        on_menu_click(&button, &popover, move || {
            if let Some(color) = color {
                let _ = set_group_color(&state, &group_id, color);
            }
        });
    }
    {
        let state = state.clone();
        on_menu_click(&delete_btn, &popover, move || {
            let _ = delete_group(&state, &group_id);
        });
    }
    popover.popup();
}

/// The "Move to group" items of a workspace's context menu.
pub(super) fn append_workspace_group_items(
    state: &State,
    workspace_id: &str,
    container: &gtk::Box,
    popover: &gtk::Popover,
) {
    let (current_group, groups) = {
        let s = state.borrow();
        let current = s
            .sidebar
            .group_of(workspace_id)
            .map(|group| group.id.clone());
        // The groups it can move to: every group but its own.
        let groups: Vec<(String, String, WorkspaceColor)> = s
            .sidebar
            .groups()
            .filter(|group| current.as_deref() != Some(group.id.as_str()))
            .map(|group| (group.id.clone(), group.name.clone(), group.color))
            .collect();
        (current, groups)
    };

    if !groups.is_empty() {
        let heading = gtk::Label::builder()
            .label("Move to group")
            .xalign(0.0)
            .margin_start(8)
            .margin_top(4)
            .build();
        heading.add_css_class("dim-label");
        heading.add_css_class("caption");
        container.append(&heading);
    }

    for (group_id, name, color) in groups {
        let dot = gtk::Label::new(Some("\u{25CF}"));
        dot.add_css_class(&format!("{DOT_CSS_PREFIX}{}", color.as_str()));
        let label = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&dot);
        content.append(&label);
        let button = gtk::Button::new();
        button.set_child(Some(&content));
        button.add_css_class("flat");
        container.append(&button);
        let state = state.clone();
        let workspace_id = workspace_id.to_string();
        on_menu_click(&button, popover, move || {
            let _ = set_workspace_group(&state, &workspace_id, Some(&group_id));
        });
    }

    let new_group_btn = menu_button("New group");
    container.append(&new_group_btn);
    {
        let state = state.clone();
        let workspace_id = workspace_id.to_string();
        on_menu_click(&new_group_btn, popover, move || {
            if let Ok(group_id) =
                create_group(&state, None, None, std::slice::from_ref(&workspace_id))
            {
                begin_group_inline_rename(&state, &group_id);
            }
        });
    }

    if current_group.is_some() {
        let remove_btn = menu_button("Remove from group");
        container.append(&remove_btn);
        let state = state.clone();
        let workspace_id = workspace_id.to_string();
        on_menu_click(&remove_btn, popover, move || {
            let _ = set_workspace_group(&state, &workspace_id, None);
        });
    }
}

fn install_group_header_interactions(state: &State, group_id: &str, row: &gtk::ListBoxRow) {
    let right_click = gtk::GestureClick::new();
    right_click.set_button(3);
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        let row = row.clone();
        right_click.connect_pressed(move |_, _, _, _| {
            show_group_context_menu(&state, &group_id, &row);
        });
    }
    row.add_controller(right_click);

    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gtk::gdk::DragAction::MOVE);
    {
        let payload = format!("{GROUP_DRAG_PREFIX}{group_id}");
        drag_source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&glib::Value::from(
                &payload,
            )))
        });
    }
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        let row = row.clone();
        drag_source.connect_drag_begin(move |source, _| {
            state.borrow_mut().group_dragging = Some(group_id.clone());
            source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
        });
    }
    {
        let state = state.clone();
        drag_source.connect_drag_end(move |_, _, _| {
            state.borrow_mut().group_dragging = None;
        });
    }
    row.add_controller(drag_source);

    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    let clear = |row: &gtk::ListBoxRow| {
        row.remove_css_class("limux-drop-above");
        row.remove_css_class("limux-drop-below");
    };
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        let row = row.clone();
        drop_target.connect_motion(move |_, _, y| {
            clear(&row);
            let accepts = {
                let s = state.borrow();
                s.workspace_dragging.is_some()
                    || s.group_dragging.as_ref().is_some_and(|id| id != &group_id)
            };
            // Tabs cannot be dropped on a group.
            if !accepts {
                return gtk::gdk::DragAction::empty();
            }
            row.add_css_class(if y < row.height() as f64 / 2.0 {
                "limux-drop-above"
            } else {
                "limux-drop-below"
            });
            gtk::gdk::DragAction::MOVE
        });
    }
    {
        let row = row.clone();
        drop_target.connect_leave(move |_| clear(&row));
    }
    {
        let state = state.clone();
        let group_id = group_id.to_string();
        let row = row.clone();
        drop_target.connect_drop(move |_, value, _, y| {
            clear(&row);
            let Ok(payload) = value.get::<String>() else {
                return false;
            };
            let placement = if y < row.height() as f64 / 2.0 {
                Placement::Before
            } else {
                Placement::After
            };
            if let Some(dragged) = payload.strip_prefix(GROUP_DRAG_PREFIX) {
                return move_group(&state, dragged, &RowRef::Group(group_id.clone()), placement);
            }
            if payload.contains(':') {
                return false;
            }
            move_workspace_to_header(&state, &payload, &group_id, placement)
        });
    }
    row.add_controller(drop_target);
}

pub(super) fn group_ref(id: &str) -> String {
    format!("group:{id}")
}

fn group_payload(group: &WorkspaceGroup) -> serde_json::Value {
    serde_json::json!({
        "id": group.id.as_str(),
        "group_id": group.id.as_str(),
        "ref": group_ref(&group.id),
        "group_ref": group_ref(&group.id),
        "name": group.name.as_str(),
        "color": group.color.as_str(),
        "collapsed": group.collapsed,
        "workspace_ids": group.members(),
        "workspace_refs": group.members().iter().map(|id| workspace_ref(id)).collect::<Vec<_>>(),
    })
}

fn bridge_error(error: GroupError) -> BridgeError {
    match error {
        GroupError::UnknownGroup | GroupError::UnknownWorkspace => {
            BridgeError::not_found(error.to_string())
        }
        GroupError::EmptyName => BridgeError::invalid_params(error.to_string()),
        GroupError::NotEmpty(_) => BridgeError::conflict(error.to_string()),
    }
}

/// Resolve a group handle: its id, or else the one group with that name.
fn resolve_group(sidebar: &SidebarLayout, handle: &str) -> Result<String, BridgeError> {
    if let Some(group) = sidebar.group(handle) {
        return Ok(group.id.clone());
    }
    let mut named = sidebar.groups().filter(|group| group.name == handle);
    match (named.next(), named.next()) {
        (Some(group), None) => Ok(group.id.clone()),
        (Some(_), Some(_)) => Err(BridgeError::invalid_params(format!(
            "several groups are named {handle:?}; use the group id"
        ))),
        (None, _) => Err(BridgeError::not_found(format!(
            "group {handle:?} not found"
        ))),
    }
}

fn group_response(state: &State, group_id: &str) -> Result<serde_json::Value, BridgeError> {
    state
        .borrow()
        .sidebar
        .group(group_id)
        .map(group_payload)
        .ok_or_else(|| bridge_error(GroupError::UnknownGroup))
}

/// Run a `workspace.group_*` / `workspace.set_group` request on the GTK loop.
pub(super) fn handle_group_request(
    state: &State,
    request: GroupRequest,
) -> Result<serde_json::Value, BridgeError> {
    let resolve = |handle: &str| resolve_group(&state.borrow().sidebar, handle);
    match request {
        GroupRequest::List => {
            let groups: Vec<serde_json::Value> =
                state.borrow().sidebar.groups().map(group_payload).collect();
            Ok(serde_json::json!({ "groups": groups }))
        }
        GroupRequest::Create {
            name,
            color,
            workspaces,
        } => {
            let members = {
                let s = state.borrow();
                workspaces
                    .iter()
                    .map(|target| {
                        workspace_index_for_target(&s, target)
                            .map(|index| s.workspaces[index].id.clone())
                            .ok_or_else(|| bridge_error(GroupError::UnknownWorkspace))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let group_id =
                create_group(state, name.as_deref(), color, &members).map_err(bridge_error)?;
            group_response(state, &group_id)
        }
        GroupRequest::Rename { group, name } => {
            let group_id = resolve(&group)?;
            rename_group(state, &group_id, &name).map_err(bridge_error)?;
            group_response(state, &group_id)
        }
        GroupRequest::SetColor { group, color } => {
            let group_id = resolve(&group)?;
            set_group_color(state, &group_id, color).map_err(bridge_error)?;
            group_response(state, &group_id)
        }
        GroupRequest::SetCollapsed { group, collapsed } => {
            let group_id = resolve(&group)?;
            set_group_collapsed(state, &group_id, collapsed).map_err(bridge_error)?;
            group_response(state, &group_id)
        }
        GroupRequest::Delete { group } => {
            let group_id = resolve(&group)?;
            delete_group(state, &group_id).map_err(bridge_error)?;
            Ok(serde_json::json!({ "deleted": group_id, "group_id": group_id }))
        }
        GroupRequest::SetWorkspaceGroup { target, group } => {
            let group_id = group.as_deref().map(resolve).transpose()?;
            let workspace_id = {
                let s = state.borrow();
                workspace_index_for_target(&s, &target).map(|index| s.workspaces[index].id.clone())
            }
            .ok_or_else(|| bridge_error(GroupError::UnknownWorkspace))?;
            set_workspace_group(state, &workspace_id, group_id.as_deref()).map_err(bridge_error)?;
            let s = state.borrow();
            s.workspaces
                .iter()
                .position(|workspace| workspace.id == workspace_id)
                .and_then(|index| workspace_payload(&s, index))
                .ok_or_else(|| bridge_error(GroupError::UnknownWorkspace))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_css_paints_pills_and_member_stripes_for_every_color() {
        let css = workspace_group_css();
        for color in WorkspaceColor::ALL {
            let class = group_color_class(color);
            assert!(css.contains(&format!(".limux-ws-group-pill.{class} {{")));
            assert!(css.contains(&format!("row.{MEMBER_CSS_CLASS}.{class} {{")));
            assert!(css.contains(&color.css_hex()));
        }
    }

    #[test]
    fn group_drag_payload_is_not_a_tab_payload() {
        let payload = format!("{GROUP_DRAG_PREFIX}0f0e-uuid");
        let (pane, _) = payload.split_once(':').unwrap();
        assert!(pane.parse::<u32>().is_err());
    }
}
