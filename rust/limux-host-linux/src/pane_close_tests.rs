use super::*;

use crate::layout_state::{SplitOrientation, SplitState};

fn pump_until(timeout: std::time::Duration, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while !done() && std::time::Instant::now() < deadline {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn pump_for(duration: std::time::Duration) {
    pump_until(duration, || false);
}

/// `root` and every widget below it.
fn widget_tree(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut widgets = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        widgets.extend(widget_tree(&widget));
        child = widget.next_sibling();
    }
    widgets
}

fn weak_refs(widgets: &[gtk::Widget]) -> Vec<glib::WeakRef<gtk::Widget>> {
    widgets.iter().map(|widget| widget.downgrade()).collect()
}

fn survivors(refs: &[glib::WeakRef<gtk::Widget>]) -> Vec<&'static str> {
    refs.iter()
        .filter_map(|weak| weak.upgrade())
        .map(|widget| widget.type_().name())
        .collect()
}

fn assert_freed(what: &str, refs: &[glib::WeakRef<gtk::Widget>]) {
    pump_until(std::time::Duration::from_secs(5), || {
        survivors(refs).is_empty()
    });
    let alive = survivors(refs);
    assert!(
        alive.is_empty(),
        "{what}: {} of {} widgets still alive: {alive:?}",
        alive.len(),
        refs.len()
    );
}

fn focus_within(state: &State, widget: &gtk::Widget) -> bool {
    let window = state.borrow().window.clone();
    gtk::prelude::GtkWindowExt::focus(&window)
        .is_some_and(|focus| &focus == widget || focus.is_ancestor(widget))
}

fn assert_focus_within(what: &str, state: &State, widget: &gtk::Widget) {
    pump_until(std::time::Duration::from_secs(5), || {
        focus_within(state, widget)
    });
    let window = state.borrow().window.clone();
    let focus = gtk::prelude::GtkWindowExt::focus(&window);
    assert!(
        focus_within(state, widget),
        "{what}: the focus is on {:?}",
        focus.map(|focus| focus.type_().name())
    );
}

fn shown(widget: &gtk::Widget) -> bool {
    widget.is_mapped() && widget.width() > 0
}

/// Right-clicks `widget` and returns the menu it opens.
fn open_context_menu(widget: &gtk::Widget) -> gtk::Popover {
    let controllers = widget.observe_controllers();
    let right_click = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
        .find(|gesture| gesture.button() == 3)
        .expect("a right-click gesture");
    right_click.emit_by_name::<()>("pressed", &[&1i32, &10.0f64, &10.0f64]);
    widget
        .last_child()
        .and_downcast::<gtk::Popover>()
        .expect("a context menu")
}

fn menu_item(menu: &gtk::Popover, label: &str) -> gtk::Button {
    widget_tree(menu.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().is_some_and(|text| text == label))
        .unwrap_or_else(|| panic!("a {label:?} menu item"))
}

/// Focuses the menu item labelled `label`, as pressing it does.
fn focus_menu_item(menu: &gtk::Popover, label: &str) -> gtk::Button {
    let item = menu_item(menu, label);
    assert!(item.grab_focus());
    item
}

fn split_options() -> SplitPaneOptions {
    SplitPaneOptions {
        initial_state: None,
        skip_default_tab: false,
        inherit_active_directory: false,
        new_pane_first: false,
        persist: false,
        suppress_initial_autostart: false,
    }
}

/// Adds a workspace, shows it, focuses its last pane, and returns its id and
/// root.
fn show_focused_workspace(state: &State, layout: LayoutNodeState) -> (String, gtk::Widget) {
    add_workspace_from_state(
        state,
        &WorkspaceState {
            color: None,
            group_id: None,
            id: None,
            name: "closed".to_string(),
            favorite: false,
            cwd: None,
            folder_path: None,
            autostart_command: None,
            layout,
        },
    );
    let (index, ws_id, root) = {
        let s = state.borrow();
        let index = s.workspaces.len() - 1;
        let ws = &s.workspaces[index];
        (index, ws.id.clone(), ws.root.clone())
    };
    select_workspace_by_index(state, index);
    let panes = || {
        let mut panes = Vec::new();
        for start in [true, false] {
            let pane = find_leaf_pane(&root, gtk::Orientation::Horizontal, start);
            if !panes.contains(&pane) {
                panes.push(pane);
            }
        }
        panes
    };
    pump_until(std::time::Duration::from_secs(5), || {
        panes().iter().all(shown)
    });
    let last = panes().pop().unwrap();
    assert!(shown(&last), "workspace panes never showed");
    assert!(pane::focus_active_tab_in_pane(&last));
    pump_for(std::time::Duration::from_millis(200));
    assert!(focus_within(state, &root), "the workspace has the focus");
    (ws_id, root)
}

// Issue #202: a closed pane stayed alive for the life of the process, held by
// its own handlers. With the focus inside, GTK 4.22 also kept what was closed
// (see `terminal::unset_focus_within`), so every close here starts focused.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn closed_tabs_panes_and_workspaces_free_their_widgets() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.PaneCloseTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let wait = std::time::Duration::from_secs(5);
    let (ws_id, first) = {
        let s = state.borrow();
        let ws = s.active_workspace().unwrap();
        let first = find_leaf_pane(&ws.root, gtk::Orientation::Horizontal, true);
        (ws.id.clone(), first)
    };
    pump_until(wait, || shown(&first));

    // A tab closed with another one left to replace it.
    pane::add_terminal_tab_to_pane(&first);
    assert!(pane::focus_active_tab_in_pane(&first));
    pump_for(std::time::Duration::from_millis(200));
    let before = widget_tree(&first);
    let tab_id = pane::active_tab_in_pane(&first).unwrap();
    assert!(pane::close_tab_in_pane(&first, &tab_id));
    // Its content leaves the stack after a frame.
    pump_for(std::time::Duration::from_millis(500));
    let kept = widget_tree(&first);
    let closed_tab: Vec<_> = before.into_iter().filter(|w| !kept.contains(w)).collect();
    assert!(
        closed_tab.iter().any(|w| w.is::<gtk::GLArea>()),
        "the tab's terminal left the pane"
    );
    let refs = weak_refs(&closed_tab);
    drop((closed_tab, kept));
    assert_freed("closed tab", &refs);

    // A pane closed through its only tab, as typing `exit` does.
    let lone = split_pane(
        &state,
        &ws_id,
        &first,
        gtk::Orientation::Horizontal,
        split_options(),
    )
    .expect("split");
    pump_until(wait, || shown(&lone));
    assert!(pane::focus_active_tab_in_pane(&lone));
    pump_for(std::time::Duration::from_millis(200));
    let refs = weak_refs(&widget_tree(&lone));
    let tab_id = pane::active_tab_in_pane(&lone).unwrap();
    assert!(pane::close_tab_in_pane(&lone, &tab_id));
    drop(lone);
    assert_freed("pane closed by its last tab", &refs);
    assert_focus_within("pane closed by its last tab", &state, &first);

    // A pane with two terminal tabs and the keybinds tab, whose tab and
    // terminal menus were used and whose tab is being renamed, closed as its
    // close button does.
    let closed = split_pane(
        &state,
        &ws_id,
        &first,
        gtk::Orientation::Horizontal,
        split_options(),
    )
    .expect("split");
    let shortcuts = state.borrow().shortcuts.clone();
    pane::add_keybind_editor_tab_to_pane(&closed, shortcuts, Rc::new(|_, _| Err(String::new())));
    pane::add_terminal_tab_to_pane(&closed);
    pump_until(wait, || shown(&closed));
    assert!(shown(&closed), "split pane never showed");
    assert_eq!(pane::tab_count_in_pane(&closed), 3);
    assert!(pane::focus_active_tab_in_pane(&closed));
    pump_for(std::time::Duration::from_millis(200));

    let terminal = widget_tree(&closed)
        .into_iter()
        .find(|w| w.is::<gtk::GLArea>() && w.is_mapped())
        .expect("the active terminal");
    // A synthetic right-click has no seat/grab under headless Weston, so the
    // production entry point correctly detaches its refused popup immediately.
    // Build the same menu with the real terminal callbacks and retain it here
    // to exercise focus restoration and lifetime cleanup on close. Refused
    // popup detachment has its own terminal regression.
    let terminal_menu = pane::terminal_handle_for_surface(&closed, None)
        .expect("the active terminal handle")
        .1
        .build_context_menu_for_test();
    terminal_menu.popup();
    focus_menu_item(&terminal_menu, "Paste");
    menu_item(&terminal_menu, "Copy Surface ID").emit_clicked();
    assert!(terminal_menu.parent().is_none(), "the terminal menu closed");
    let toast = widget_tree(&closed)
        .into_iter()
        .find(|widget| widget.has_css_class("limux-toast"))
        .expect("copying the surface ID showed a toast")
        .downgrade();
    assert!(
        focus_within(&state, &terminal),
        "the terminal menu gives the focus back to its terminal as it closes"
    );

    let tab = widget_tree(&closed)
        .into_iter()
        .find(|w| w.has_css_class("limux-tab"))
        .expect("a tab button");
    let pin_menu = open_context_menu(&tab);
    focus_menu_item(&pin_menu, "Pin").emit_clicked();
    assert!(pin_menu.parent().is_none(), "the tab menu closed");
    assert_focus_within("tab menu closed", &state, &closed);
    let tab_menu = open_context_menu(&tab);
    focus_menu_item(&tab_menu, "Rename").emit_clicked();
    assert!(tab_menu.parent().is_none(), "the tab menu closed");
    pump_until(wait, || pane::find_tab_rename_entry(&closed).is_some());
    assert!(
        pane::find_tab_rename_entry(&closed).is_some(),
        "a rename is open"
    );

    let mut refs = weak_refs(&widget_tree(&closed));
    refs.push(toast);
    refs.push(terminal_menu.upcast::<gtk::Widget>().downgrade());
    refs.push(tab_menu.upcast::<gtk::Widget>().downgrade());
    refs.push(pin_menu.upcast::<gtk::Widget>().downgrade());
    drop((terminal, tab));
    remove_pane(&state, &ws_id, &closed);
    drop(closed);
    assert_freed("closed pane", &refs);
    assert_focus_within("closed pane", &state, &first);

    // Two panes closed in one tick, as two shells exiting together: the
    // focus stays where it was.
    let doomed: Vec<_> = (0..2)
        .map(|_| {
            split_pane(
                &state,
                &ws_id,
                &first,
                gtk::Orientation::Horizontal,
                split_options(),
            )
            .expect("split")
        })
        .collect();
    pump_until(wait, || doomed.iter().all(shown));
    assert!(pane::focus_active_tab_in_pane(&first));
    pump_for(std::time::Duration::from_millis(200));
    assert!(focus_within(&state, &first), "the first pane has the focus");
    let refs = weak_refs(&doomed.iter().flat_map(widget_tree).collect::<Vec<_>>());
    for pane in &doomed {
        remove_pane(&state, &ws_id, pane);
    }
    drop(doomed);
    assert_freed("two panes closed in one tick", &refs);
    assert_focus_within("two panes closed in one tick", &state, &first);

    // Workspaces: split, then single-pane, closed directly and through the
    // last terminal's tab. The focus goes back to where it was in the
    // workspace shown instead: `other`, not its first pane.
    let other = split_pane(
        &state,
        &ws_id,
        &first,
        gtk::Orientation::Horizontal,
        split_options(),
    )
    .expect("split");
    pump_until(wait, || shown(&other));
    assert!(pane::focus_active_tab_in_pane(&other));
    pump_for(std::time::Duration::from_millis(200));
    drop(first);
    let split = LayoutNodeState::Split(SplitState {
        orientation: SplitOrientation::Horizontal,
        ratio: 0.5,
        start: Box::new(LayoutNodeState::Pane(PaneState::fallback(None))),
        end: Box::new(LayoutNodeState::Pane(PaneState::fallback(None))),
    });
    let single = || LayoutNodeState::Pane(PaneState::fallback(None));
    for (what, layout, by_last_tab) in [
        ("closed split workspace", split, false),
        ("closed single-pane workspace", single(), false),
        ("workspace closed by its last tab", single(), true),
    ] {
        let (ws_id, root) = show_focused_workspace(&state, layout);
        let refs = weak_refs(&widget_tree(&root));
        if by_last_tab {
            let pane = find_leaf_pane(&root, gtk::Orientation::Horizontal, true);
            let tab_id = pane::active_tab_in_pane(&pane).unwrap();
            assert!(pane::close_tab_in_pane(&pane, &tab_id));
        } else {
            close_workspace_by_id(&state, &ws_id);
        }
        drop(root);
        assert_freed(what, &refs);
        assert_focus_within(what, &state, &other);
    }

    // A workspace kept open after its last terminal exits: the pane stays,
    // the terminal goes although it had the focus.
    state
        .borrow()
        .config
        .borrow_mut()
        .workspace
        .keep_open_after_last_terminal_closes = true;
    let (_, root) = show_focused_workspace(&state, single());
    let kept_pane = find_leaf_pane(&root, gtk::Orientation::Horizontal, true);
    let before = widget_tree(&kept_pane);
    let tab_id = pane::active_tab_in_pane(&kept_pane).unwrap();
    assert!(pane::close_tab_in_pane(&kept_pane, &tab_id));
    pump_for(std::time::Duration::from_millis(500));
    assert!(kept_pane.parent().is_some(), "the pane was kept");
    let kept = widget_tree(&kept_pane);
    let closed_tab: Vec<_> = before.into_iter().filter(|w| !kept.contains(w)).collect();
    assert!(
        closed_tab.iter().any(|w| w.is::<gtk::GLArea>()),
        "the terminal left the kept pane"
    );
    let refs = weak_refs(&closed_tab);
    drop((closed_tab, kept, kept_pane, root));
    assert_freed("terminal of a kept-open workspace", &refs);

    let window = state.borrow().window.clone();
    window.close();
}

fn registered_panes() -> Vec<gtk::Widget> {
    (0..1000).filter_map(pane::find_pane_widget_by_id).collect()
}

/// Splits `first` with a new pane before it, shows both and focuses the new
/// one, so closing it closes the first leaf of the tree.
fn split_before(state: &State, ws_id: &str, first: &gtk::Widget) -> gtk::Widget {
    let lead = split_pane(
        state,
        ws_id,
        first,
        gtk::Orientation::Horizontal,
        SplitPaneOptions {
            new_pane_first: true,
            ..split_options()
        },
    )
    .expect("split");
    pump_until(std::time::Duration::from_secs(5), || {
        shown(&lead) && shown(first)
    });
    assert!(shown(&lead) && shown(first), "split panes never showed");
    assert!(pane::focus_active_tab_in_pane(&lead));
    pump_for(std::time::Duration::from_millis(200));
    lead
}

// A zoom or split within one frame of a pane close used to target the closed
// pane: the old widget tree stays in the workspace until the next frame, and
// its first leaf was found when the focus sat in no pane.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn zoom_and_split_within_a_frame_of_a_close_skip_the_closed_pane() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.PaneCloseZoomSplitTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let wait = std::time::Duration::from_secs(5);
    let (ws_id, first, container) = {
        let s = state.borrow();
        let ws = s.active_workspace().unwrap();
        let first = find_leaf_pane(&ws.root, gtk::Orientation::Horizontal, true);
        (ws.id.clone(), first, ws.split_container.clone())
    };
    pump_until(wait, || shown(&first));

    // Zoom right after the first leaf closed.
    let lead = split_before(&state, &ws_id, &first);
    remove_pane(&state, &ws_id, &lead);
    toggle_focused_pane_zoom(&state);
    pump_until(wait, || shown(&first));
    assert!(shown(&first), "the zoom left the workspace blank");
    assert!(!container.contains(&lead), "the closed pane is in the tree");
    toggle_focused_pane_zoom(&state);
    pump_for(std::time::Duration::from_millis(300));
    drop(lead);

    // Directional lookups skip it too.
    let lead = split_before(&state, &ws_id, &first);
    remove_pane(&state, &ws_id, &lead);
    assert!(
        pane_in_direction(&state, &first, Direction::Left).is_none(),
        "found the closed pane"
    );
    drop(lead);
    pump_for(std::time::Duration::from_millis(300));

    // A split of the closed pane itself builds nothing.
    let lead = split_before(&state, &ws_id, &first);
    remove_pane(&state, &ws_id, &lead);
    let before = registered_panes().len();
    assert!(
        split_pane(
            &state,
            &ws_id,
            &lead,
            gtk::Orientation::Horizontal,
            split_options()
        )
        .is_none(),
        "split a closed pane"
    );
    assert_eq!(registered_panes().len(), before, "an orphaned pane");
    drop(lead);
    pump_for(std::time::Duration::from_millis(300));

    // Split right after the first leaf closed.
    let lead = split_before(&state, &ws_id, &first);
    remove_pane(&state, &ws_id, &lead);
    let before = registered_panes().len();
    split_focused_pane(&state, gtk::Orientation::Horizontal);
    pump_for(std::time::Duration::from_millis(500));
    let panes = registered_panes();
    assert_eq!(panes.len(), before + 1, "the split built one pane");
    assert!(
        panes.iter().all(shown),
        "a registered pane is not in the tree"
    );

    let window = state.borrow().window.clone();
    window.close();
}

// The workspace menu's items held their own popover, a cycle that kept it and
// its row parent chain alive after every use. The headless compositor has no
// seat, so the popover is never presented nor closed: detach it by hand, as
// its `closed` handler would, and require that nothing else retains it.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn workspace_context_menu_never_retains_its_popover() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.WorkspaceMenuTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let row = state.borrow().workspaces[0].sidebar_row.clone();
    pump_for(std::time::Duration::from_millis(200));

    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Rename").emit_clicked();
    let refs = vec![menu.clone().upcast::<gtk::Widget>().downgrade()];
    menu.unparent();
    drop(menu);
    assert_freed("workspace context menu", &refs);
}

// Starts an inline rename with the focus in a terminal and commits it with
// Enter, as a user would: the entry is removed while it holds the focus, so
// unless the commit moves the focus first, GTK hands it to the next focusable
// widget in the row, the star button.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn inline_rename_commit_returns_the_focus_to_the_workspace() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.InlineRenameFocusTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));

    begin_workspace_inline_rename(&state, &ws_id);
    let sidebar_list = state.borrow().sidebar_list.clone();
    let entry = find_active_rename_entry(&sidebar_list).expect("rename entry");
    assert_focus_within("rename started", &state, entry.upcast_ref());

    entry.set_text("renamed");
    entry.emit_activate();
    assert_focus_within("rename committed", &state, &root);
    {
        let s = state.borrow();
        let ws = s.workspaces.iter().find(|ws| ws.id == ws_id).unwrap();
        assert_eq!(ws.name, "renamed");
    }
    assert!(find_active_rename_entry(&sidebar_list).is_none());

    // A click on a terminal commits the rename on press (the window's
    // capture-phase `click_anywhere`), then the terminal takes the focus. The
    // commit must leave it there, not pull it to the workspace's first pane.
    let split = LayoutNodeState::Split(SplitState {
        orientation: SplitOrientation::Horizontal,
        ratio: 0.5,
        start: Box::new(LayoutNodeState::Pane(PaneState::fallback(None))),
        end: Box::new(LayoutNodeState::Pane(PaneState::fallback(None))),
    });
    let (ws_id, root) = show_focused_workspace(&state, split);
    let clicked = find_leaf_pane(&root, gtk::Orientation::Horizontal, false);
    begin_workspace_inline_rename(&state, &ws_id);
    let entry = find_active_rename_entry(&sidebar_list).expect("rename entry");
    assert_focus_within("split rename started", &state, entry.upcast_ref());
    entry.emit_activate();
    assert!(find_gl_area(&clicked).expect("terminal").grab_focus());
    pump_for(std::time::Duration::from_millis(200));
    assert_focus_within("rename committed by a click", &state, &clicked);
}

#[test]
#[ignore = "requires an active X11 window and Ghostty resources"]
fn passive_rename_focus_restore_preserves_manual_tab_unread() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);
    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.UnreadFocusRestoreTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let tab_id = pane::active_tab_in_pane(&pane).unwrap();
    let (window, row, sidebar_list) = {
        let s = state.borrow();
        let ws = s.workspaces.iter().find(|ws| ws.id == ws_id).unwrap();
        (
            s.window.clone(),
            ws.sidebar_row.clone(),
            s.sidebar_list.clone(),
        )
    };
    let wait = std::time::Duration::from_secs(5);
    pump_until(wait, || window.is_active());
    assert!(window.is_active(), "Limux must start active");
    assert!(pane::mark_active_tab_manual_unread_in_root(&root));
    sync_workspace_unread(&state, &ws_id);

    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Rename").emit_clicked();
    pump_until(wait, || find_active_rename_entry(&sidebar_list).is_some());
    let entry = find_active_rename_entry(&sidebar_list).expect("rename entry");
    assert_focus_within("rename started", &state, entry.upcast_ref());
    entry.set_text("renamed while inactive");

    let peer = adw::ApplicationWindow::builder()
        .application(&app)
        .title("Focus peer")
        .build();
    peer.present();
    pump_until(wait, || peer.is_active() && !window.is_active());
    assert!(peer.is_active() && !window.is_active());
    assert!(find_active_rename_entry(&sidebar_list).is_none());
    assert!(state.borrow().pending_focus_restore.is_some());
    assert!(pane::workspace_has_unread_tabs(&ws_id));
    assert_eq!(
        state.borrow().active_workspace().unwrap().name,
        entry.text()
    );

    window.present();
    pump_until(wait, || window.is_active());
    assert!(window.is_active());
    assert_focus_within("passive activation restores terminal focus", &state, &pane);
    assert!(state.borrow().pending_focus_restore.is_none());
    assert!(
        pane::workspace_has_unread_tabs(&ws_id),
        "passive rename focus restoration must preserve a manual tab marker"
    );
    assert!(pane::activate_tab_in_pane(&pane, &tab_id));
    assert!(
        pane::workspace_has_unread_tabs(&ws_id),
        "showing the tab keeps its marker until the user types"
    );
    peer.close();
    window.close();
}

#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn tab_unread_changes_preserve_independent_workspace_notifications() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.UnreadSourcesTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    pane::add_terminal_tab_to_pane(&pane);
    let surface = pane::active_surface_summary(&pane).unwrap();
    let (_, handle) = pane::terminal_handle_for_surface(&pane, Some(&surface.surface_id)).unwrap();
    let wait = std::time::Duration::from_secs(5);
    pump_until(wait, || handle.health().realized);
    assert!(handle.health().realized);
    select_workspace_by_index(&state, 0);

    let target = DesktopNotificationTarget {
        workspace_id: ws_id.clone(),
        pane_id: None,
        tab_id: None,
    };
    mark_workspace_unread_with_message(&state, &ws_id, "Workspace notice", false, target);
    let target = DesktopNotificationTarget {
        workspace_id: ws_id.clone(),
        pane_id: Some(surface.pane_id),
        tab_id: Some(surface.tab_id.clone()),
    };
    mark_workspace_unread_with_message(&state, &ws_id, "Tab notice", false, target);
    assert!(pane::workspace_has_unread_tabs(&ws_id));
    assert!(handle.send_text("exit"));
    assert!(handle.send_key("enter"));
    // Nobody watched it exit, so the tab and its output stay until closed.
    pump_until(wait, || handle.health().process_exited);
    pump_for(std::time::Duration::from_millis(300));
    assert_eq!(
        pane::tab_count_in_pane(&pane),
        2,
        "an unattended exit keeps its tab"
    );
    assert!(pane::close_tab_in_pane(&pane, &surface.tab_id));
    pump_until(wait, || pane::tab_count_in_pane(&pane) == 1);
    assert_eq!(pane::tab_count_in_pane(&pane), 1, "unread terminal exited");
    let (row, notify_dot) = {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        assert!(
            workspace.unread,
            "a terminal exit must preserve the independent workspace notice"
        );
        (
            workspace.sidebar_row.clone(),
            workspace.indicator_unread_dot.clone(),
        )
    };
    assert!(!pane::workspace_has_unread_tabs(&ws_id));
    assert!(notify_dot.get_visible());

    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Mark as Read").emit_clicked();
    pump_until(wait, || !notify_dot.get_visible());
    assert!(!notify_dot.get_visible());
    let index = state
        .borrow()
        .workspaces
        .iter()
        .position(|w| w.id == ws_id)
        .unwrap();
    select_workspace_by_index(&state, index);
    pane::add_terminal_tab_to_pane(&pane);
    assert!(pane::mark_active_tab_manual_unread_in_root(&root));
    sync_workspace_unread(&state, &ws_id);
    assert!(pane::workspace_has_unread_tabs(&ws_id));
    assert!(notify_dot.get_visible());
    assert!(pane::workspace_has_unread_tabs(&ws_id));
    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Mark as Read").emit_clicked();
    pump_until(wait, || !pane::workspace_has_unread_tabs(&ws_id));
    assert!(!pane::workspace_has_unread_tabs(&ws_id));
    assert!(
        !notify_dot.get_visible(),
        "reading a tab-only notice clears it"
    );

    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Mark as Unread").emit_clicked();
    pump_until(wait, || notify_dot.get_visible());
    {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        assert!(workspace.manual_unread);
        assert!(
            !workspace.unread,
            "manual and notification sources are separate"
        );
    }
    let surface = pane::active_surface_summary(&pane).unwrap();
    let (_, handle) = pane::terminal_handle_for_surface(&pane, Some(&surface.surface_id)).unwrap();
    pump_until(wait, || handle.health().realized);
    assert!(handle.send_text("exit"));
    assert!(handle.send_key("enter"));
    pump_until(wait, || pane::tab_count_in_pane(&pane) == 1);
    assert_eq!(pane::tab_count_in_pane(&pane), 1);
    assert!(
        state.borrow().workspaces[index].manual_unread,
        "a terminal exit must preserve the independent manual workspace reminder"
    );
    assert!(notify_dot.get_visible());
    let menu = open_context_menu(row.upcast_ref());
    menu_item(&menu, "Mark as Read").emit_clicked();
    pump_until(wait, || !notify_dot.get_visible());
    assert!(!notify_dot.get_visible());
    assert!(!pane::workspace_has_unread_tabs(&ws_id));

    pane::add_terminal_tab_to_pane(&pane);
    let tab_id = pane::active_tab_in_pane(&pane).unwrap();
    let pane_id = pane::active_surface_summary(&pane).unwrap().pane_id;
    assert_eq!(
        pane::mark_tab_unread_in_workspace(&ws_id, pane_id, &tab_id),
        Some(true)
    );
    sync_workspace_unread(&state, &ws_id);
    assert!(notify_dot.get_visible());
    assert!(pane::close_tab_in_pane(&pane, &tab_id));
    assert!(!pane::workspace_has_unread_tabs(&ws_id));
    assert!(
        !notify_dot.get_visible(),
        "closing a tab-only notice clears it"
    );

    let window = state.borrow().window.clone();
    window.close();
    pump_until(wait, || !window.is_visible());
}

fn buttons_labelled(root: &gtk::Widget, label: &str) -> Vec<gtk::Button> {
    widget_tree(root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .filter(|button| button.label().is_some_and(|text| text == label))
        .collect()
}

fn live_terminal(pane: &gtk::Widget, surface_id: &str) -> bool {
    pane::terminal_handle_for_surface(pane, Some(surface_id))
        .is_some_and(|(_, handle)| handle.health().realized && !handle.health().process_exited)
}

// A process that ends in a workspace nobody is looking at keeps the workspace,
// its group place and the terminal's output, marks it unread and offers a
// restart in place.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn unattended_exit_keeps_the_workspace_and_restarts_in_place() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.UnattendedExitTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let original = pane::active_tab_in_pane(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    assert!(pane::close_tab_in_pane(&pane, &original));
    pump_until(wait, || pane::tab_count_in_pane(&pane) == 1);
    let surface = pane::active_surface_summary(&pane).unwrap();
    pump_until(wait, || live_terminal(&pane, &surface.surface_id));
    let (_, handle) = pane::terminal_handle_for_surface(&pane, Some(&surface.surface_id)).unwrap();
    select_workspace_by_index(&state, 0);

    assert!(handle.send_text("exit 3"));
    assert!(handle.send_key("enter"));
    pump_until(wait, || handle.health().process_exited);
    pump_until(wait, || !buttons_labelled(&root, "Restart").is_empty());
    // The unread mark is applied from an idle callback.
    pump_for(std::time::Duration::from_millis(200));
    let (kept, message) = {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id);
        (
            workspace.is_some(),
            workspace.map(|w| w.notify_label.label().to_string()),
        )
    };
    assert!(
        kept,
        "the only terminal exiting unseen must not close the workspace"
    );
    assert_eq!(message.as_deref(), Some("Process exited with code 3"));
    assert_eq!(pane::tab_count_in_pane(&pane), 1, "the exited tab is held");
    assert!(pane::workspace_has_unread_tabs(&ws_id));

    buttons_labelled(&root, "Restart")[0].emit_clicked();
    pump_until(wait, || live_terminal(&pane, &surface.surface_id));
    assert!(
        live_terminal(&pane, &surface.surface_id),
        "restart runs a new process in the same tab"
    );
    pump_until(wait, || buttons_labelled(&root, "Restart").is_empty());
    assert!(buttons_labelled(&root, "Restart").is_empty());

    // Close on the held banner closes the tab, and with it the workspace.
    let (_, handle) = pane::terminal_handle_for_surface(&pane, Some(&surface.surface_id)).unwrap();
    assert!(handle.send_text("exit 4"));
    assert!(handle.send_key("enter"));
    pump_until(wait, || !buttons_labelled(&root, "Close").is_empty());
    buttons_labelled(&root, "Close")[0].emit_clicked();
    pump_until(wait, || {
        !state.borrow().workspaces.iter().any(|w| w.id == ws_id)
    });
    assert!(!state.borrow().workspaces.iter().any(|w| w.id == ws_id));

    let window = state.borrow().window.clone();
    window.close();
}

// A terminal that exits in a background tab is held; opening that tab later
// puts the keyboard on Restart, not on the dead terminal.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn opening_a_held_background_tab_focuses_restart() {
    let (state, _temp) = held_tab_test_window("dev.limux.HeldBackgroundTabTest");
    let wait = std::time::Duration::from_secs(5);

    let (_ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let background = pane::active_surface_summary(&pane).unwrap();
    pump_until(wait, || live_terminal(&pane, &background.surface_id));
    let (_, handle) =
        pane::terminal_handle_for_surface(&pane, Some(&background.surface_id)).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    let front = pane::active_surface_summary(&pane).unwrap();
    assert_ne!(front.tab_id, background.tab_id);
    pump_for(std::time::Duration::from_millis(300));

    assert!(handle.send_text("exit 5"));
    assert!(handle.send_key("enter"));
    pump_until(wait, || handle.health().process_exited);
    pump_until(wait, || !buttons_labelled(&root, "Restart").is_empty());
    pump_for(std::time::Duration::from_millis(200));
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(front.tab_id.as_str()),
        "the exit happened in a background tab"
    );

    assert!(pane::activate_tab_in_pane(&pane, &background.tab_id));
    let restart = buttons_labelled(&root, "Restart")[0].clone();
    pump_until(wait, || restart.has_focus());
    assert!(
        restart.has_focus(),
        "opening a held tab focuses its Restart button"
    );

    let window = state.borrow().window.clone();
    window.close();
}

fn held_tab_test_window(app_id: &str) -> (State, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder().application_id(app_id).build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    (state, temp)
}

// Returning to a workspace lands in the pane last used there, not its first
// pane, and the recent-workspace action toggles between the last two.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn returning_to_a_workspace_focuses_its_last_pane() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.RecentWorkspaceTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let first = first_leaf_pane(&root);
    let second = split_pane(
        &state,
        &ws_id,
        &first,
        gtk::Orientation::Horizontal,
        split_options(),
    )
    .expect("split");
    pump_until(wait, || shown(&second));
    assert!(pane::focus_active_tab_in_pane(&second));
    assert_focus_within("the second pane", &state, &second);
    let index = state
        .borrow()
        .workspaces
        .iter()
        .position(|w| w.id == ws_id)
        .unwrap();

    activate_workspace_shortcut(&state, 0);
    pump_for(std::time::Duration::from_millis(200));
    assert_eq!(state.borrow().active_idx, 0);
    assert_eq!(switch_to_recent_workspace(&state), Some(index));
    assert_focus_within("the last used pane after returning", &state, &second);
    assert_eq!(switch_to_recent_workspace(&state), Some(0));
    // The shortcut goes through the same action.
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::RecentWorkspace
    ));
    assert_eq!(state.borrow().active_idx, index);
    assert_focus_within("the last used pane after toggling", &state, &second);

    let window = state.borrow().window.clone();
    window.close();
}

// The next-unread action reaches a tab in a hidden workspace and focuses it.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn next_unread_terminal_reaches_a_hidden_tab() {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder()
        .application_id("dev.limux.NextUnreadTest")
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    pane::add_terminal_tab_to_pane(&pane);
    let background = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    activate_workspace_shortcut(&state, 0);
    pump_for(std::time::Duration::from_millis(200));
    assert_eq!(
        pane::mark_tab_unread_in_workspace(&ws_id, background.pane_id, &background.tab_id),
        Some(true)
    );
    sync_workspace_unread(&state, &ws_id);

    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::NextUnreadTerminal
    ));
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(background.tab_id.as_str())
            && focus_within(&state, &pane)
    });
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(background.tab_id.as_str())
    );
    assert_focus_within("the unread tab", &state, &pane);
    assert!(
        pane::workspace_has_unread_tabs(&ws_id),
        "viewing it keeps the mark until the user types"
    );

    let window = state.borrow().window.clone();
    window.close();
}

fn pending_test_window(app_id: &str) -> (State, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let path = temp.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    let ghostty_config = temp.path().join("XDG_CONFIG_HOME/ghostty");
    std::fs::create_dir_all(&ghostty_config).unwrap();
    std::fs::write(
        ghostty_config.join("config"),
        "command = /bin/sh\nshell-integration = none\n",
    )
    .unwrap();
    let socket = temp.path().join("limux.sock");
    std::env::set_var("LIMUX_SOCKET", &socket);
    std::env::set_var("LIMUX_SOCKET_PATH", &socket);

    crate::prepare_ghostty_runtime();
    adw::init().unwrap();
    crate::terminal::init_ghostty();
    let app = adw::Application::builder().application_id(app_id).build();
    app.register(None::<&gio::Cancellable>).unwrap();
    build_window(&app);
    let state = CONTROL_STATE.with(|slot| slot.borrow().as_ref().unwrap().clone());
    (state, temp)
}

fn pending_counter(state: &State) -> gtk::Button {
    state.borrow().top_bar_pending_btn.clone().unwrap()
}

// A finished terminal in a background tab shows in the counter and the pending
// panel; picking it focuses that exact tab, and viewing it reads it.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn pending_panel_lists_a_finished_terminal_and_jumps_to_it() {
    let (state, _temp) = pending_test_window("dev.limux.PendingPanelTest");
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    pane::add_terminal_tab_to_pane(&pane);
    let finished = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    activate_workspace_shortcut(&state, 0);
    pump_for(std::time::Duration::from_millis(1500));
    assert!(!pending_counter(&state).is_visible(), "nothing pending yet");

    mark_workspace_unread_with_message(
        &state,
        &ws_id,
        "Claude finished: tests pass",
        false,
        DesktopNotificationTarget {
            workspace_id: ws_id.clone(),
            pane_id: Some(finished.pane_id),
            tab_id: Some(finished.tab_id.clone()),
        },
    );
    let counter = pending_counter(&state);
    assert!(counter.is_visible());
    assert_eq!(pending_panel::counter_text(&state), "\u{25CF} 1");

    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(wait, || pending_panel::open_entry_count(&state).is_some());
    assert_eq!(pending_panel::open_entry_count(&state), Some(1));

    pending_panel::activate_entry(&state, 0);
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(finished.tab_id.as_str())
            && focus_within(&state, &pane)
            && pending_panel::open_entry_count(&state).is_none()
    });
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(finished.tab_id.as_str())
    );
    assert_focus_within("the finished tab", &state, &pane);
    assert_eq!(
        pending_panel::open_entry_count(&state),
        None,
        "the panel closed"
    );
    pump_for(std::time::Duration::from_millis(1500));
    assert!(
        pane::workspace_has_unread_tabs(&ws_id),
        "viewing it keeps the mark"
    );
    pane::simulate_typing_in_tab(&finished.tab_id);
    pump_until(wait, || !counter.is_visible());
    assert!(
        !pane::workspace_has_unread_tabs(&ws_id),
        "typing into it reads it"
    );
    assert!(!counter.is_visible(), "nothing pending any more");

    let window = state.borrow().window.clone();
    window.close();
}

// A zoom keeps the other panes out of the workspace root. A finished terminal
// in such a pane still counts, is listed, and "Mark all read" reaches it.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn pending_panel_reaches_a_pane_hidden_by_zoom() {
    let (state, _temp) = pending_test_window("dev.limux.PendingZoomTest");
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let kept = first_leaf_pane(&root);
    let hidden = split_before(&state, &ws_id, &kept);
    let finished = pane::active_surface_summary(&hidden).unwrap();
    let (container, root) = {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        (workspace.split_container.clone(), workspace.root.clone())
    };
    assert!(container.toggle_zoom(&kept));
    pump_until(wait, || !hidden.is_ancestor(&root));
    assert!(!hidden.is_ancestor(&root), "the zoom hid the other pane");

    mark_workspace_unread_with_message(
        &state,
        &ws_id,
        "Claude finished behind the zoom",
        false,
        DesktopNotificationTarget {
            workspace_id: ws_id.clone(),
            pane_id: Some(finished.pane_id),
            tab_id: Some(finished.tab_id.clone()),
        },
    );
    pump_for(std::time::Duration::from_millis(1500));
    let counter = pending_counter(&state);
    assert!(counter.is_visible(), "the hidden pane counts");
    assert_eq!(pending_panel::counter_text(&state), "\u{25CF} 1");

    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(wait, || pending_panel::open_entry_count(&state).is_some());
    assert_eq!(pending_panel::open_entry_count(&state), Some(1));

    pending_panel::click_mark_all(&state);
    pump_until(wait, || !pane::workspace_has_unread_tabs(&ws_id));
    assert!(
        !pane::workspace_has_unread_tabs(&ws_id),
        "mark all read reached the hidden pane"
    );
    assert!(!counter.is_visible(), "nothing pending any more");

    let window = state.borrow().window.clone();
    window.close();
}

// With the top bar and the sidebar hidden, the counter moves next to the
// sidebar toggle in the pane header, the panel still opens, and "Mark all
// read" leaves a waiting terminal in place.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn pending_panel_works_with_both_bars_hidden() {
    let (state, _temp) = pending_test_window("dev.limux.PendingPanelHiddenTest");
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let waiting = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    let finished = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    activate_workspace_shortcut(&state, 0);
    pump_for(std::time::Duration::from_millis(1500));

    agent_activity_ui::handle_agent_activity(
        &state,
        &WorkspaceTarget::Active,
        &waiting.tab_id,
        crate::agent_activity::AgentEvent {
            tab_id: String::new(),
            agent: "claude".to_string(),
            phase: Some(crate::agent_activity::AgentPhase::NeedsApproval),
            session_id: None,
            at_ms: crate::agent_activity::now_ms(),
            detail: Some("Bash: cargo test".to_string()),
        },
    )
    .unwrap();
    mark_workspace_unread_with_message(
        &state,
        &ws_id,
        "Claude finished",
        false,
        DesktopNotificationTarget {
            workspace_id: ws_id.clone(),
            pane_id: Some(finished.pane_id),
            tab_id: Some(finished.tab_id.clone()),
        },
    );

    // Hide both bars the way the user does.
    toggle_top_bar(&state);
    toggle_sidebar(&state);
    pump_for(std::time::Duration::from_millis(800));
    let root = state.borrow().active_workspace().unwrap().root.clone();
    let counter = pending_counter(&state);
    assert!(!state.borrow().top_bar.as_ref().unwrap().is_visible());
    assert!(
        counter.is_ancestor(&root),
        "with both bars hidden the counter sits in the pane header"
    );
    assert!(counter.is_visible());
    assert_eq!(
        pending_panel::counter_text(&state).as_str(),
        "\u{25C6} 1 \u{25CF} 1",
        "waiting and unread show together"
    );

    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(wait, || pending_panel::open_entry_count(&state).is_some());
    assert_eq!(pending_panel::open_entry_count(&state), Some(2));

    // Mark all read: the finished one turns quiet (still listed until it is
    // closed, but not counted), the waiting one stays.
    pending_panel::click_mark_all(&state);
    pump_until(wait, || pending_panel::counter_text(&state) == "\u{25C6} 1");
    assert_eq!(pending_panel::counter_text(&state), "\u{25C6} 1");
    assert_eq!(pending_panel::open_entry_count(&state), Some(2));
    assert!(!pane::workspace_has_unread_tabs(&ws_id));

    // Ctrl+Alt+P again closes it.
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(wait, || pending_panel::open_entry_count(&state).is_none());
    assert_eq!(pending_panel::open_entry_count(&state), None);

    let window = state.borrow().window.clone();
    window.close();
}

// A plain terminal that prints on its own in a background tab is listed as
// working, turns unread when it stops, keeps that mark through a visit until
// the user types, then stays listed as quiet until it is closed. Ctrl+Alt+O
// goes back to the terminal used before.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn background_terminal_is_kept_until_closed() {
    let (state, _temp) = pending_test_window("dev.limux.KeptTerminalTest");
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let front = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    let back = pane::active_surface_summary(&pane).unwrap();
    let (_, back_handle) = pane::terminal_tabs_in_pane(&pane)
        .into_iter()
        .find(|(tab_id, _)| *tab_id == back.tab_id)
        .unwrap();
    let go_to = |tab_id: &str| {
        activate_desktop_notification_target(
            &state,
            &DesktopNotificationTarget {
                workspace_id: ws_id.clone(),
                pane_id: Some(front.pane_id),
                tab_id: Some(tab_id.to_string()),
            },
            None,
        )
    };
    // Past the shell's start-up, then the command runs in the background.
    pump_for(std::time::Duration::from_millis(3200));
    assert!(back_handle.send_text(
        "sleep 2; i=0; while [ $i -lt 6 ]; do echo tick$i; i=$((i+1)); sleep 0.3; done"
    ));
    assert!(back_handle.send_key("Enter"));
    go_to(&front.tab_id);
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(front.tab_id.as_str())
    });
    let phase = |tab_id: &str| state.borrow().agent_activity.phase(tab_id);
    let long = std::time::Duration::from_secs(10);
    pump_until(long, || {
        phase(&back.tab_id) == Some(crate::agent_activity::AgentPhase::Working)
    });
    assert_eq!(
        phase(&back.tab_id),
        Some(crate::agent_activity::AgentPhase::Working),
        "output of its own makes it working; screen: {:?}",
        back_handle.read_viewport_text()
    );
    assert_eq!(
        phase(&front.tab_id),
        None,
        "the tab the user only looked at is not listed"
    );
    assert_eq!(
        agent_activity_ui::workspace_activity_counts(&state.borrow(), &ws_id).working,
        1
    );

    // It stops while off screen: finished and unread.
    pump_until(long, || pane::workspace_has_unread_tabs(&ws_id));
    assert!(pane::workspace_has_unread_tabs(&ws_id));
    assert_eq!(
        phase(&back.tab_id),
        Some(crate::agent_activity::AgentPhase::Idle)
    );
    assert_eq!(pending_panel::counter_text(&state), "\u{25CF} 1");

    // A visit keeps the mark; typing reads it, and it stays listed.
    go_to(&back.tab_id);
    pump_for(std::time::Duration::from_millis(1500));
    assert!(pane::workspace_has_unread_tabs(&ws_id), "a look keeps it");
    pane::simulate_typing_in_tab(&back.tab_id);
    pump_for(std::time::Duration::from_millis(200));
    assert!(!pane::workspace_has_unread_tabs(&ws_id), "typing reads it");
    assert_eq!(
        agent_activity_ui::workspace_activity_counts(&state.borrow(), &ws_id).quiet,
        1,
        "kept as quiet"
    );

    // Ctrl+Alt+O goes back to the terminal used before, and again forth.
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::RecentWorkspace
    ));
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(front.tab_id.as_str())
    });
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(front.tab_id.as_str())
    );
    pump_for(std::time::Duration::from_millis(200));
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::RecentWorkspace
    ));
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(back.tab_id.as_str())
    });
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(back.tab_id.as_str())
    );

    // Closing it from the list drops it until it does something again.
    agent_activity_ui::dismiss_terminal(&state, &back.tab_id);
    assert_eq!(phase(&back.tab_id), None);
    pump_for(std::time::Duration::from_millis(1500));
    assert_eq!(phase(&back.tab_id), None, "it stays closed while quiet");

    let window = state.borrow().window.clone();
    window.close();
}

/// Save `widget` as a PNG when LIMUX_TEST_SNAPSHOT_DIR is set (manual review).
fn snapshot_widget(widget: &gtk::Widget, name: &str) {
    let Some(dir) = std::env::var_os("LIMUX_TEST_SNAPSHOT_DIR") else {
        return;
    };
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    // A dark backdrop, as popovers draw on their own (transparent) surface.
    snapshot.append_color(
        &gtk::gdk::RGBA::new(0.12, 0.12, 0.13, 1.0),
        &gtk::graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32),
    );
    paintable.snapshot(&snapshot, widget.width() as f64, widget.height() as f64);
    let Some(node) = snapshot.to_node() else {
        return;
    };
    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .unwrap();
    let texture = renderer.render_texture(&node, None);
    texture
        .save_to_png(std::path::Path::new(&dir).join(format!("{name}.png")))
        .unwrap();
}

// The ▸ on a project row lists its terminals as tabs: one chip per terminal,
// the one on screen marked, background ones dimmed, and a click focuses it.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn session_rows_list_terminals_as_tabs() {
    let (state, _temp) = pending_test_window("dev.limux.SessionRowsTest");
    let wait = std::time::Duration::from_secs(5);

    let (ws_id, root) =
        show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    let pane = first_leaf_pane(&root);
    let first = pane::active_surface_summary(&pane).unwrap();
    pane::add_terminal_tab_to_pane(&pane);
    let second = pane::active_surface_summary(&pane).unwrap();
    activate_workspace_shortcut(&state, 0);
    pump_for(std::time::Duration::from_millis(500));
    agent_activity_ui::handle_agent_activity(
        &state,
        &WorkspaceTarget::Active,
        &first.tab_id,
        crate::agent_activity::AgentEvent {
            tab_id: String::new(),
            agent: "claude".to_string(),
            phase: Some(crate::agent_activity::AgentPhase::Working),
            session_id: None,
            at_ms: crate::agent_activity::now_ms(),
            detail: Some("Bash: cargo test".to_string()),
        },
    )
    .unwrap();

    let (toggle, sessions, sidebar) = {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        (
            workspace.sessions_toggle.clone(),
            workspace.sessions_box.clone(),
            s.sidebar_shell.clone(),
        )
    };
    let chips = |sessions: &gtk::Box| -> Vec<gtk::Button> {
        widget_tree(sessions.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .filter(|button| button.has_css_class("limux-session-row"))
            .collect()
    };
    // Closed, a single active terminal puts what it does under the project
    // name, with no chip.
    pump_for(std::time::Duration::from_millis(300));
    assert!(sessions.is_visible(), "an active terminal shows without ▸");
    assert!(
        chips(&sessions).is_empty(),
        "one active terminal needs no chip"
    );
    let lines: Vec<String> = widget_tree(sessions.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .filter(|label| label.has_css_class("limux-ws-activity-line"))
        .map(|label| label.label().to_string())
        .collect();
    assert_eq!(lines, ["Bash: cargo test"]);
    snapshot_widget(sidebar.upcast_ref(), "session-rows-closed");

    toggle.emit_clicked();
    pump_for(std::time::Duration::from_millis(300));
    assert!(sessions.is_visible());
    let rows = chips(&sessions);
    assert_eq!(rows.len(), 2, "▸ shows every terminal");
    assert!(
        rows[1].has_css_class("limux-session-row-shown"),
        "the tab on screen is marked"
    );
    assert!(
        rows[0].has_css_class("limux-session-row-background"),
        "the background tab is dimmed"
    );
    snapshot_widget(sidebar.upcast_ref(), "session-rows");

    rows[0].emit_clicked();
    pump_until(wait, || {
        pane::active_tab_in_pane(&pane).as_deref() == Some(first.tab_id.as_str())
    });
    assert_eq!(
        pane::active_tab_in_pane(&pane).as_deref(),
        Some(first.tab_id.as_str())
    );
    let _ = second;

    let window = state.borrow().window.clone();
    window.close();
}

/// Adds a named one-pane workspace and returns its id and pane.
fn add_named_workspace(state: &State, name: &str) -> (String, gtk::Widget) {
    add_workspace_from_state(
        state,
        &WorkspaceState {
            color: None,
            group_id: None,
            id: None,
            name: name.to_string(),
            favorite: false,
            cwd: None,
            folder_path: None,
            autostart_command: None,
            layout: LayoutNodeState::Pane(PaneState::fallback(None)),
        },
    );
    let s = state.borrow();
    let workspace = s.workspaces.last().unwrap();
    let pane = first_leaf_pane(&workspace.root);
    (workspace.id.clone(), pane)
}

fn report(
    state: &State,
    ws_id: &str,
    tab_id: &str,
    phase: crate::agent_activity::AgentPhase,
    detail: &str,
) {
    agent_activity_ui::handle_agent_activity(
        state,
        &WorkspaceTarget::Handle(ws_id.to_string()),
        tab_id,
        crate::agent_activity::AgentEvent {
            tab_id: String::new(),
            agent: "claude".to_string(),
            phase: Some(phase),
            session_id: None,
            at_ms: crate::agent_activity::now_ms(),
            detail: (!detail.is_empty()).then(|| detail.to_string()),
        },
    )
    .unwrap();
}

// A collapsed group's `● N` goes to an unread terminal even while another
// waits, and `+N more` focuses the terminal that put a project there, here a
// quiet agent in a background tab.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn group_counts_and_overflow_reach_the_right_terminal() {
    use crate::agent_activity::AgentPhase::{Idle, NeedsApproval, Working};
    let (state, _temp) = pending_test_window("dev.limux.GroupJumpTest");
    let wait = std::time::Duration::from_secs(5);

    let (home_id, _) = add_named_workspace(&state, "home");
    let mut members = Vec::new();
    let mut tabs = Vec::new();
    for name in ["waits", "finished", "w1", "w2", "w3", "quiet"] {
        let (id, pane) = add_named_workspace(&state, name);
        tabs.push(pane::active_surface_summary(&pane).unwrap());
        if name == "quiet" {
            // The quiet agent sits in a background tab.
            pane::add_terminal_tab_to_pane(&pane);
        }
        members.push(id);
    }
    let group = sidebar_groups::create_group(&state, Some("G"), None, &members).unwrap();
    sidebar_groups::set_group_collapsed(&state, &group, true).unwrap();
    let index_of = |id: &str| {
        state
            .borrow()
            .workspaces
            .iter()
            .position(|w| w.id == id)
            .unwrap()
    };
    select_workspace_by_index(&state, index_of(&home_id));
    pump_for(std::time::Duration::from_millis(500));

    report(
        &state,
        &members[0],
        &tabs[0].tab_id,
        NeedsApproval,
        "Bash: ls",
    );
    mark_workspace_unread_with_message(
        &state,
        &members[1],
        "Claude finished",
        false,
        DesktopNotificationTarget {
            workspace_id: members[1].clone(),
            pane_id: Some(tabs[1].pane_id),
            tab_id: Some(tabs[1].tab_id.clone()),
        },
    );
    for i in 2..5 {
        report(&state, &members[i], &tabs[i].tab_id, Working, "Edit: x");
    }
    report(&state, &members[5], &tabs[5].tab_id, Idle, "");
    pump_for(std::time::Duration::from_millis(300));

    let active_id = || state.borrow().active_workspace().unwrap().id.clone();
    sidebar_groups::click_unread_count(&state, &group);
    pump_until(wait, || active_id() == members[1]);
    assert_eq!(
        active_id(),
        members[1],
        "the unread count reaches the finished project, not the waiting one"
    );

    select_workspace_by_index(&state, index_of(&home_id));
    pump_for(std::time::Duration::from_millis(300));
    sidebar_groups::click_more(&state, &group);
    let quiet_pane = first_leaf_pane(&state.borrow().workspaces[index_of(&members[5])].root);
    pump_until(wait, || {
        active_id() == members[5]
            && pane::active_tab_in_pane(&quiet_pane).as_deref() == Some(tabs[5].tab_id.as_str())
    });
    assert_eq!(active_id(), members[5], "+1 more goes to the quiet project");
    assert_eq!(
        pane::active_tab_in_pane(&quiet_pane).as_deref(),
        Some(tabs[5].tab_id.as_str()),
        "and to its quiet terminal, not the tab in front"
    );

    let window = state.borrow().window.clone();
    window.close();
}

// A note that arrives while the agent works (a background agent finished)
// does not stop the spinner: the project keeps showing working, and its `●`
// shows once the agent stops.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn a_note_while_working_keeps_the_spinner_until_it_stops() {
    use crate::agent_activity::AgentPhase::{Idle, Working};
    let (state, _temp) = pending_test_window("dev.limux.NoteWhileWorkingTest");
    let wait = std::time::Duration::from_secs(5);

    let (home_id, _) = add_named_workspace(&state, "home");
    let (ws_id, pane) = add_named_workspace(&state, "AyahLoop");
    let tab = pane::active_surface_summary(&pane).unwrap();
    let index = state
        .borrow()
        .workspaces
        .iter()
        .position(|w| w.id == home_id)
        .unwrap();
    select_workspace_by_index(&state, index);
    pump_for(std::time::Duration::from_millis(300));

    report(&state, &ws_id, &tab.tab_id, Working, "Poll");
    mark_workspace_unread_with_message(
        &state,
        &ws_id,
        "Claude: ca842437 finished",
        false,
        DesktopNotificationTarget {
            workspace_id: ws_id.clone(),
            pane_id: Some(tab.pane_id),
            tab_id: Some(tab.tab_id.clone()),
        },
    );
    pump_for(std::time::Duration::from_millis(300));
    let (spinner, dot) = {
        let s = state.borrow();
        let workspace = s.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        (
            workspace.row_extras.spinner.clone(),
            workspace.notify_dot.clone(),
        )
    };
    assert!(pane::workspace_has_unread_tabs(&ws_id), "the note is kept");
    assert!(spinner.is_visible(), "the row keeps its spinner");
    assert!(!dot.is_visible(), "no dot while it works");
    assert_eq!(
        pending_panel::counter_text(&state),
        format!("{} 1", crate::agent_activity::GLYPH_WORKING)
    );

    report(&state, &ws_id, &tab.tab_id, Idle, "");
    pump_until(wait, || dot.is_visible());
    assert!(
        dot.is_visible(),
        "once it stops, the note shows as finished"
    );
    assert!(!spinner.is_visible());
    assert_eq!(pending_panel::counter_text(&state), "\u{25CF} 1");

    let window = state.borrow().window.clone();
    window.close();
}

// Not a check: builds a realistic sidebar (two groups, one collapsed, agents
// working, waiting and finished) and saves pictures of each view for a UX
// review. Run with LIMUX_TEST_SNAPSHOT_DIR set.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn ux_tour_snapshots() {
    use crate::agent_activity::AgentPhase::*;
    let (state, _temp) = pending_test_window("dev.limux.UxTour");
    let window = state.borrow().window.clone();
    window.set_default_size(1280, 800);

    let clients = [
        ("api-server", Some(Working), "Bash: cargo test --workspace"),
        ("billing", Some(NeedsApproval), "Bash: psql -f migrate.sql"),
        ("docs-site", None, ""),
        ("mobile-app", None, "finished"),
        ("infra", Some(Working), "Edit: main.tf"),
        ("search", Some(Working), "Grep: fn rank"),
    ];
    let mut client_ids = Vec::new();
    let mut reports = Vec::new();
    for (name, phase, detail) in clients {
        let (id, pane) = add_named_workspace(&state, name);
        let tab = pane::active_surface_summary(&pane).unwrap();
        client_ids.push(id.clone());
        reports.push((id, tab, phase, detail));
    }
    let (limux_id, limux_pane) = add_named_workspace(&state, "limux");
    let limux_first = pane::active_surface_summary(&limux_pane).unwrap();
    pane::add_terminal_tab_to_pane(&limux_pane);
    let limux_second = pane::active_surface_summary(&limux_pane).unwrap();
    pane::add_terminal_tab_to_pane(&limux_pane);
    let limux_shell = pane::active_surface_summary(&limux_pane).unwrap();
    let (ghostty_id, ghostty_pane) = add_named_workspace(&state, "ghostty");
    let ghostty_tab = pane::active_surface_summary(&ghostty_pane).unwrap();
    let (notes_id, notes_pane) = add_named_workspace(&state, "notes");
    let notes_tab = pane::active_surface_summary(&notes_pane).unwrap();
    pump_until(std::time::Duration::from_secs(5), || {
        live_terminal(&notes_pane, &notes_tab.surface_id)
    });

    let clients_group = sidebar_groups::create_group(
        &state,
        Some("Clients"),
        Some(WorkspaceColor::Blue),
        &client_ids,
    )
    .unwrap();
    sidebar_groups::create_group(
        &state,
        Some("Limux"),
        Some(WorkspaceColor::Teal),
        &[limux_id.clone(), ghostty_id.clone()],
    )
    .unwrap();
    sidebar_groups::set_group_collapsed(&state, &clients_group, true).unwrap();
    let limux_index = state
        .borrow()
        .workspaces
        .iter()
        .position(|w| w.id == limux_id)
        .unwrap();
    select_workspace_by_index(&state, limux_index);
    pump_for(std::time::Duration::from_millis(800));

    for (id, tab, phase, detail) in &reports {
        match phase {
            Some(phase) => report(&state, id, &tab.tab_id, *phase, detail),
            None if *detail == "finished" => {
                mark_workspace_unread_with_message(
                    &state,
                    id,
                    "Claude finished: all 214 tests pass",
                    false,
                    DesktopNotificationTarget {
                        workspace_id: id.clone(),
                        pane_id: Some(tab.pane_id),
                        tab_id: Some(tab.tab_id.clone()),
                    },
                );
            }
            None => report(&state, id, &tab.tab_id, Idle, ""),
        }
    }
    report(
        &state,
        &limux_id,
        &limux_first.tab_id,
        Working,
        "Bash: ./scripts/check.sh",
    );
    report(
        &state,
        &limux_id,
        &limux_second.tab_id,
        NeedsInput,
        "Which branch should I rebase onto?",
    );
    mark_workspace_unread_with_message(
        &state,
        &ghostty_id,
        "Claude finished: zig build passed",
        false,
        DesktopNotificationTarget {
            workspace_id: ghostty_id.clone(),
            pane_id: Some(ghostty_tab.pane_id),
            tab_id: Some(ghostty_tab.tab_id.clone()),
        },
    );
    // A plain shell that finished a build and was read: kept as quiet.
    state
        .borrow_mut()
        .agent_activity
        .keep(&limux_shell.tab_id, crate::agent_activity::now_ms());
    pending_panel::note_message(
        &mut state.borrow_mut(),
        &limux_id,
        Some(&limux_shell.tab_id),
        "Finished",
    );
    // The notes shell exits while nobody watches: held with Restart / Close.
    let (_, notes_handle) =
        pane::terminal_handle_for_surface(&notes_pane, Some(&notes_tab.surface_id)).unwrap();
    assert!(notes_handle.send_text("exit 2"));
    assert!(notes_handle.send_key("enter"));
    pump_until(std::time::Duration::from_secs(5), || {
        notes_handle.health().process_exited
    });
    agent_activity_ui::refresh_activity(&state);
    pump_for(std::time::Duration::from_millis(1500));
    let content = window.child().unwrap();
    snapshot_widget(&content, "tour-1-overview");
    let sidebar = state.borrow().sidebar_shell.clone();
    snapshot_widget(sidebar.upcast_ref(), "tour-2-sidebar");

    let toggle = {
        let s = state.borrow();
        s.workspaces
            .iter()
            .find(|w| w.id == limux_id)
            .unwrap()
            .sessions_toggle
            .clone()
    };
    toggle.emit_clicked();
    pump_for(std::time::Duration::from_millis(400));
    snapshot_widget(sidebar.upcast_ref(), "tour-3-sidebar-all-terminals");
    toggle.emit_clicked();

    sidebar_groups::set_group_collapsed(&state, &clients_group, false).unwrap();
    pump_for(std::time::Duration::from_millis(400));
    snapshot_widget(sidebar.upcast_ref(), "tour-4-sidebar-group-expanded");
    sidebar_groups::set_group_collapsed(&state, &clients_group, true).unwrap();
    pump_for(std::time::Duration::from_millis(400));

    // Test terminals print nothing, so agents settle to idle after 10 s of
    // silence; report the working ones again before the panel.
    for (id, tab, phase, detail) in &reports {
        if *phase == Some(Working) {
            report(&state, id, &tab.tab_id, Working, detail);
        }
    }
    report(
        &state,
        &limux_id,
        &limux_first.tab_id,
        Working,
        "Bash: ./scripts/check.sh",
    );
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(std::time::Duration::from_secs(5), || {
        pending_panel::open_entry_count(&state).is_some()
    });
    pump_for(std::time::Duration::from_millis(400));
    let popover = pending_panel::open_popover(&state).unwrap();
    snapshot_widget(popover.upcast_ref(), "tour-5-pending-panel");
    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_for(std::time::Duration::from_millis(300));

    toggle_top_bar(&state);
    toggle_sidebar(&state);
    pump_for(std::time::Duration::from_millis(800));
    snapshot_widget(&content, "tour-7-full-screen");
    toggle_top_bar(&state);
    toggle_sidebar(&state);

    // The held exit, opened.
    let notes_index = state
        .borrow()
        .workspaces
        .iter()
        .position(|w| w.id == notes_id)
        .unwrap();
    select_workspace_by_index(&state, notes_index);
    pump_for(std::time::Duration::from_millis(800));
    snapshot_widget(&content, "tour-10-held-exit");
    let _ = (limux_second, limux_pane);
    window.close();
}

// Claude Code is set up here but without Limux hooks: the sidebar says how to
// add them. The empty agents panel explains the signs.
#[test]
#[ignore = "requires a graphical display and Ghostty resources"]
fn hooks_card_and_empty_panel_explain_themselves() {
    let claude = tempfile::tempdir().unwrap();
    std::fs::write(claude.path().join("settings.json"), r#"{"model":"opus"}"#).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", claude.path());
    let (state, _temp) = pending_test_window("dev.limux.HooksCardTest");
    show_focused_workspace(&state, LayoutNodeState::Pane(PaneState::fallback(None)));
    pump_for(std::time::Duration::from_millis(500));

    let sidebar = state.borrow().sidebar_shell.clone();
    let card = widget_tree(sidebar.upcast_ref())
        .into_iter()
        .find(|widget| widget.has_css_class("limux-hooks-card"))
        .expect("the hooks card is in the sidebar");
    assert!(
        card.is_visible(),
        "Claude without Limux hooks shows the card"
    );
    snapshot_widget(sidebar.upcast_ref(), "tour-8-no-hooks");

    assert!(dispatch_shortcut_command(
        &state,
        ShortcutCommand::PendingPanel
    ));
    pump_until(std::time::Duration::from_secs(5), || {
        pending_panel::open_entry_count(&state).is_some()
    });
    assert_eq!(pending_panel::open_entry_count(&state), Some(0));
    pump_for(std::time::Duration::from_millis(300));
    let popover = pending_panel::open_popover(&state).unwrap();
    let legend = widget_tree(popover.upcast_ref())
        .into_iter()
        .any(|widget| widget.has_css_class("limux-pending-legend"));
    assert!(legend, "the empty panel shows the legend");
    snapshot_widget(popover.upcast_ref(), "tour-9-empty-panel");

    let window = state.borrow().window.clone();
    window.close();
}
