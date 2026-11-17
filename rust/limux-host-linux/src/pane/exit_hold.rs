//! What happens to a terminal tab when its process ends on its own.
//!
//! A terminal closes when the user ends it in front of them, as in every
//! terminal. A process that ends while nobody is watching (a hidden project,
//! a background tab, an inactive window) keeps its tab, its output and its
//! project, with a banner to restart or close it. A process that fails right
//! after starting is held even in front of the user, so a mistyped command
//! does not flash and vanish.

use super::*;
use std::time::Duration;

/// Matches Ghostty's `abnormal-command-exit-runtime` default.
pub(super) const ABNORMAL_EXIT_RUNTIME: Duration = Duration::from_millis(250);

pub(super) const EXITED_TAB_CSS_CLASS: &str = "limux-tab-exited";
const EXIT_BANNER_CSS_CLASS: &str = "limux-exit-banner";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExitDisposition {
    /// Remove the tab as before (and the project with its last terminal,
    /// unless the keep-open setting is on).
    Close,
    /// Keep the tab with its output and show the exit banner. `notify` marks
    /// the tab and project unread because the user did not see it happen.
    Hold { notify: bool },
}

/// `code` is `None` when the surface asked to close itself rather than its
/// child exiting; that is always a close. `attended` means the tab is the
/// active tab of a pane shown in the active project of the active window.
pub(super) fn exit_disposition(
    code: Option<u32>,
    attended: bool,
    runtime: Duration,
) -> ExitDisposition {
    let Some(code) = code else {
        return ExitDisposition::Close;
    };
    if !attended {
        return ExitDisposition::Hold { notify: true };
    }
    if code != 0 && runtime <= ABNORMAL_EXIT_RUNTIME {
        return ExitDisposition::Hold { notify: false };
    }
    ExitDisposition::Close
}

pub(super) fn exit_message(code: u32) -> String {
    if code == 0 {
        "Process exited".to_string()
    } else {
        format!("Process exited with code {code}")
    }
}

pub(super) struct ExitBanner {
    pub(super) root: gtk::Widget,
    pub(super) restart_button: gtk::Button,
}

pub(super) fn build_exit_banner(
    message: &str,
    on_restart: impl Fn() + 'static,
    on_close: impl Fn() + 'static,
) -> ExitBanner {
    let banner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    banner.add_css_class(EXIT_BANNER_CSS_CLASS);
    banner.add_css_class("osd");
    banner.add_css_class("toolbar");
    banner.set_halign(gtk::Align::Center);
    banner.set_valign(gtk::Align::End);
    banner.set_margin_bottom(16);

    let label = gtk::Label::new(Some(message));
    label.set_margin_start(6);
    banner.append(&label);

    let restart_button = gtk::Button::with_label("Restart");
    restart_button.set_tooltip_text(Some("Start the same command again in this tab"));
    restart_button.connect_clicked(move |_| on_restart());
    banner.append(&restart_button);

    let close_button = gtk::Button::with_label("Close");
    close_button.set_tooltip_text(Some("Close this tab"));
    close_button.connect_clicked(move |_| on_close());
    banner.append(&close_button);

    ExitBanner {
        root: banner.upcast(),
        restart_button,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(40);

    #[test]
    fn attended_exit_closes_like_any_terminal() {
        assert_eq!(
            exit_disposition(Some(0), true, LONG),
            ExitDisposition::Close
        );
        assert_eq!(
            exit_disposition(Some(1), true, LONG),
            ExitDisposition::Close
        );
        assert_eq!(
            exit_disposition(Some(0), true, SHORT),
            ExitDisposition::Close
        );
    }

    #[test]
    fn unattended_exit_is_held_and_notifies() {
        for code in [0, 1, 129] {
            for runtime in [SHORT, LONG] {
                assert_eq!(
                    exit_disposition(Some(code), false, runtime),
                    ExitDisposition::Hold { notify: true }
                );
            }
        }
    }

    #[test]
    fn attended_failure_right_after_start_is_held_quietly() {
        assert_eq!(
            exit_disposition(Some(127), true, SHORT),
            ExitDisposition::Hold { notify: false }
        );
        assert_eq!(
            exit_disposition(Some(127), true, ABNORMAL_EXIT_RUNTIME),
            ExitDisposition::Hold { notify: false }
        );
    }

    #[test]
    fn surface_close_request_always_closes() {
        assert_eq!(exit_disposition(None, false, LONG), ExitDisposition::Close);
        assert_eq!(exit_disposition(None, true, SHORT), ExitDisposition::Close);
    }

    #[test]
    fn exit_message_names_non_zero_codes() {
        assert_eq!(exit_message(0), "Process exited");
        assert_eq!(exit_message(2), "Process exited with code 2");
    }
}
