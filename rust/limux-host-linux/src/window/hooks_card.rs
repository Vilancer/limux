//! A card at the bottom of the sidebar when Claude Code is set up on this
//! machine but its Limux hooks are not: without them the sidebar shows no
//! agent signs, and nothing else would say why. It names the one command to
//! run, and can be dismissed for good.

use super::*;

const SETUP_COMMAND: &str = "limux hooks setup claude";
const DISMISSED_FILE: &str = "claude-hooks-card-dismissed";
/// How often the card checks again, so it goes away once the hooks are in.
const RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// The Claude config directory Claude Code itself uses.
fn claude_config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| Some(glib::home_dir().join(".claude")))
}

fn dismissed_marker() -> std::path::PathBuf {
    // Next to the saved session, so a test's own session dir keeps it too.
    crate::layout_state::persistence_dir().join(DISMISSED_FILE)
}

/// Whether to suggest the setup: Claude Code has a config directory here, and
/// its settings carry no Limux hook (`limux ... hooks claude ...`).
pub(super) fn hooks_missing(config_dir_exists: bool, settings: Option<&str>) -> bool {
    config_dir_exists && !settings.is_some_and(|text| text.contains("hooks claude"))
}

fn should_show() -> bool {
    if dismissed_marker().exists() {
        return false;
    }
    let Some(dir) = claude_config_dir() else {
        return false;
    };
    let settings = std::fs::read_to_string(dir.join("settings.json")).ok();
    hooks_missing(dir.is_dir(), settings.as_deref())
}

pub(super) fn build() -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("limux-hooks-card");

    let title = gtk::Label::new(Some("Agent signs need Claude Code hooks"));
    title.add_css_class("limux-hooks-card-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    let text = gtk::Label::new(Some(
        "Run this once, then restart your Claude Code sessions:",
    ));
    text.set_xalign(0.0);
    text.set_wrap(true);
    let command = gtk::Label::new(Some(SETUP_COMMAND));
    command.add_css_class("limux-hooks-card-command");
    command.set_xalign(0.0);
    command.set_selectable(true);

    let copy = gtk::Button::with_label("Copy command");
    copy.add_css_class("flat");
    copy.add_css_class("limux-hooks-card-button");
    let dismiss = gtk::Button::with_label("Dismiss");
    dismiss.add_css_class("flat");
    dismiss.add_css_class("limux-hooks-card-button");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.append(&copy);
    buttons.append(&dismiss);

    card.append(&title);
    card.append(&text);
    card.append(&command);
    card.append(&buttons);
    card.set_visible(should_show());

    copy.connect_clicked(|button| {
        button.clipboard().set_text(SETUP_COMMAND);
        button.set_label("Copied");
    });
    {
        let card = card.downgrade();
        dismiss.connect_clicked(move |_| {
            let marker = dismissed_marker();
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, b"");
            if let Some(card) = card.upgrade() {
                card.set_visible(false);
            }
        });
    }
    {
        let card = card.downgrade();
        glib::timeout_add_local(RECHECK_INTERVAL, move || {
            let Some(card) = card.upgrade() else {
                return glib::ControlFlow::Break;
            };
            card.set_visible(should_show());
            glib::ControlFlow::Continue
        });
    }
    card
}

pub(super) fn hooks_card_css() -> &'static str {
    r#"
.limux-hooks-card {
    margin: 6px 8px;
    padding: 8px 10px;
    border-radius: 8px;
    font-size: 11.5px;
    background: alpha(@window_fg_color, 0.07);
    border: 1px dashed alpha(@window_fg_color, 0.3);
}
.limux-hooks-card-title {
    font-weight: 700;
}
.limux-hooks-card-command {
    margin: 4px 0;
    padding: 3px 6px;
    border-radius: 5px;
    background: alpha(black, 0.3);
    font-family: monospace;
    font-size: 11px;
}
.limux-hooks-card-button {
    min-height: 0;
    padding: 1px 6px;
    font-size: 11px;
}
"#
}

#[cfg(test)]
mod tests {
    use super::hooks_missing;

    #[test]
    fn suggests_setup_only_for_claude_without_limux_hooks() {
        // No Claude Code here: nothing to suggest.
        assert!(!hooks_missing(false, None));
        // Claude Code without settings, or settings without the hook.
        assert!(hooks_missing(true, None));
        assert!(hooks_missing(true, Some(r#"{"model":"opus"}"#)));
        // Installed by `limux hooks setup claude`.
        assert!(!hooks_missing(
            true,
            Some(
                r#"{"hooks":{"Stop":[{"hooks":[{"command":"limux --json hooks claude stop"}]}]}}"#
            )
        ));
    }
}
