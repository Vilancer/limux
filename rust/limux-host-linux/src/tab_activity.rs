use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::app_config::InterfaceConfig;
use crate::terminal::TerminalInput;

#[derive(Clone)]
pub(crate) struct TabActivity(Rc<ActivityState>);

struct ActivityState {
    spinner: gtk::Spinner,
    last_output: Cell<Option<Instant>>,
    /// Any output, seen or not, for telling a quiet agent from a busy one.
    last_any_output: Cell<Option<Instant>>,
    acknowledged_through: Cell<Instant>,
    /// Output before this answers the user (the echo of a key, a redraw
    /// after a resize or focus change), not the terminal doing something.
    quiet_until: Cell<Instant>,
    /// While the shell starts (its greeting, a slow config, a prompt that
    /// fills in its git status), until this or until input is sent to it.
    starting_until: Cell<Option<Instant>>,
    /// The current run of output the terminal made on its own: (first, last).
    own_output: Cell<Option<(Instant, Instant)>>,
    mapped: Cell<bool>,
    unread: Cell<bool>,
    enabled: Cell<bool>,
    timeout: Cell<Duration>,
    expiry: RefCell<Option<glib::SourceId>>,
}

impl TabActivity {
    pub(crate) fn new(config: &InterfaceConfig) -> Self {
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(12, 12);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_can_target(false);
        spinner.set_visible(false);
        spinner.set_tooltip_text(Some("Recent background terminal output"));
        spinner.update_property(&[gtk::accessible::Property::Label(
            "Recent background terminal output",
        )]);
        spinner.add_css_class("limux-tab-activity");
        Self(Rc::new(ActivityState {
            spinner,
            last_output: Cell::new(None),
            last_any_output: Cell::new(None),
            acknowledged_through: Cell::new(Instant::now()),
            quiet_until: Cell::new(Instant::now()),
            // The terminal sets this again when its surface (and shell)
            // starts; a restored background tab starts when first shown.
            starting_until: Cell::new(Some(Instant::now() + STARTUP_WINDOW)),
            own_output: Cell::new(None),
            mapped: Cell::new(false),
            unread: Cell::new(false),
            enabled: Cell::new(config.show_tab_activity),
            timeout: Cell::new(Duration::from_secs(u64::from(config.tab_activity_seconds))),
            expiry: RefCell::new(None),
        }))
    }

    pub(crate) fn widget(&self) -> &gtk::Spinner {
        &self.0.spinner
    }

    pub(crate) fn watch_content(&self, content: &impl IsA<gtk::Widget>) {
        self.set_mapped(content.is_mapped());
        let weak = Rc::downgrade(&self.0);
        content.connect_map(move |_| {
            if let Some(state) = weak.upgrade() {
                Self(state).set_mapped(true);
            }
        });
        let weak = Rc::downgrade(&self.0);
        content.connect_unmap(move |_| {
            if let Some(state) = weak.upgrade() {
                Self(state).set_mapped(false);
            }
        });
    }

    fn set_mapped(&self, mapped: bool) {
        if self.0.mapped.replace(mapped) != mapped {
            self.clear();
        }
    }

    pub(crate) fn set_unread(&self, unread: bool) {
        if self.0.unread.replace(unread) != unread {
            self.clear();
        }
    }

    fn clear(&self) {
        // PTY delivery is coalesced, so already-seen output may still be queued.
        self.0.acknowledged_through.set(Instant::now());
        self.0.last_output.set(None);
        self.sync();
    }

    /// Input reached the terminal, or its shell started. Output in the next
    /// moment is its answer, not activity of its own. Its start-up output
    /// counts only from the first typed or sent input, or once it is still
    /// going after the start-up window (a long autostart command).
    pub(crate) fn note_input(&self, at: Instant, input: TerminalInput) {
        let until = at + INPUT_GRACE;
        if until > self.0.quiet_until.get() {
            self.0.quiet_until.set(until);
        }
        self.0
            .starting_until
            .set(starting_until(self.0.starting_until.get(), at, input));
        if input == TerminalInput::Start {
            self.0.own_output.set(None);
        }
    }

    pub(crate) fn record_output(&self, at: Instant) {
        if self.0.last_any_output.get().is_none_or(|last| at > last) {
            self.0.last_any_output.set(Some(at));
        }
        if self.0.enabled.get()
            && at >= self.0.quiet_until.get()
            && self.0.starting_until.get().is_none_or(|until| at >= until)
        {
            self.0.own_output.set(extend_run(
                self.0.own_output.get(),
                at,
                self.0.timeout.get(),
            ));
        }
        if self.0.enabled.get()
            && !self.0.mapped.get()
            && !self.0.unread.get()
            && at > self.0.acknowledged_through.get()
        {
            self.0.last_output.set(Some(at));
            self.sync();
        }
    }

    /// How long the terminal has had no output, or `None` when output is not
    /// tracked (the tab activity setting is off). Counts from `since` (the
    /// terminal's start) when it never printed.
    pub(crate) fn silent_for(&self, now: Instant, since: Instant) -> Option<Duration> {
        if !self.0.enabled.get() {
            return None;
        }
        let last = self.0.last_any_output.get().unwrap_or(since);
        Some(now.saturating_duration_since(last))
    }

    /// When the terminal started printing on its own, while it still does
    /// (output within the activity timeout); `None` when it is quiet or
    /// output is not tracked.
    pub(crate) fn working_since(&self, now: Instant) -> Option<Instant> {
        if !self.0.enabled.get() {
            return None;
        }
        running_since(self.0.own_output.get(), self.0.timeout.get(), now)
    }

    pub(crate) fn configure(&self, config: &InterfaceConfig) {
        self.0.enabled.set(config.show_tab_activity);
        self.0
            .timeout
            .set(Duration::from_secs(u64::from(config.tab_activity_seconds)));
        if !config.show_tab_activity {
            self.0.acknowledged_through.set(Instant::now());
            self.0.last_output.set(None);
        }
        self.cancel_expiry();
        self.sync();
    }

    fn cancel_expiry(&self) {
        if let Some(source) = self.0.expiry.borrow_mut().take() {
            source.remove();
        }
    }

    fn sync(&self) {
        let remaining = remaining_activity(
            self.0.enabled.get(),
            self.0.last_output.get(),
            self.0.timeout.get(),
            Instant::now(),
        );
        let Some(remaining) = remaining else {
            self.0.spinner.stop();
            self.0.spinner.set_visible(false);
            self.cancel_expiry();
            return;
        };
        self.0.spinner.set_visible(true);
        self.0.spinner.start();
        if self.0.expiry.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let source =
                glib::timeout_add_local_once(remaining.max(Duration::from_millis(1)), move || {
                    if let Some(state) = weak.upgrade() {
                        state.expiry.borrow_mut().take();
                        Self(state).sync();
                    }
                });
            *self.0.expiry.borrow_mut() = Some(source);
        }
    }
}

impl Drop for ActivityState {
    fn drop(&mut self) {
        if let Some(source) = self.expiry.get_mut().take() {
            source.remove();
        }
        self.spinner.stop();
    }
}

/// How long after input (a key, a resize, a focus change) output still
/// counts as the terminal's answer to it.
const INPUT_GRACE: Duration = Duration::from_secs(1);
/// How long after the terminal starts its output is the shell starting up,
/// unless input is sent to it sooner. Shells that print a system summary or
/// draw their prompt in the background take several seconds, more while a
/// restore starts many at once.
const STARTUP_WINDOW: Duration = Duration::from_secs(15);

/// The end of the start-up window after `input` at `at`.
fn starting_until(current: Option<Instant>, at: Instant, input: TerminalInput) -> Option<Instant> {
    match input {
        TerminalInput::Start => Some(at + STARTUP_WINDOW),
        // Someone is using the shell: what it prints now is theirs.
        TerminalInput::Typed | TerminalInput::Sent => None,
        TerminalInput::Redraw => current,
    }
}

/// Add output at `at` to the current run, or start a new run when the last
/// output is older than `gap`.
fn extend_run(
    run: Option<(Instant, Instant)>,
    at: Instant,
    gap: Duration,
) -> Option<(Instant, Instant)> {
    match run {
        Some((first, last)) if at.saturating_duration_since(last) < gap => {
            Some((first, last.max(at)))
        }
        _ => Some((at, at)),
    }
}

fn running_since(run: Option<(Instant, Instant)>, gap: Duration, now: Instant) -> Option<Instant> {
    run.filter(|(_, last)| now.saturating_duration_since(*last) < gap)
        .map(|(first, _)| first)
}

fn remaining_activity(
    enabled: bool,
    last_output: Option<Instant>,
    timeout: Duration,
    now: Instant,
) -> Option<Duration> {
    if !enabled {
        return None;
    }
    last_output
        .and_then(|at| timeout.checked_sub(now.saturating_duration_since(at)))
        .filter(|remaining| !remaining.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_up_output_counts_only_after_input_or_the_window() {
        let start = Instant::now();
        let s = |secs: u64| start + Duration::from_secs(secs);
        let started = starting_until(None, s(0), TerminalInput::Start);
        assert_eq!(started, Some(s(15)));
        // A resize or focus change while it starts keeps the window.
        assert_eq!(
            starting_until(started, s(2), TerminalInput::Redraw),
            started
        );
        // A typed or sent command ends it.
        assert_eq!(starting_until(started, s(4), TerminalInput::Typed), None);
        assert_eq!(starting_until(started, s(4), TerminalInput::Sent), None);
        // A later restart opens it again.
        assert_eq!(
            starting_until(None, s(30), TerminalInput::Start),
            Some(s(45))
        );
    }

    #[test]
    fn own_output_runs_until_it_stops_for_the_timeout() {
        let start = Instant::now();
        let gap = Duration::from_secs(5);
        let s = |secs: u64| start + Duration::from_secs(secs);
        let run = extend_run(None, s(0), gap);
        let run = extend_run(run, s(3), gap);
        assert_eq!(running_since(run, gap, s(4)), Some(s(0)));
        assert_eq!(running_since(run, gap, s(8)), None, "quiet for the timeout");
        // Output after the pause starts a new run.
        let run = extend_run(run, s(20), gap);
        assert_eq!(running_since(run, gap, s(21)), Some(s(20)));
    }

    #[test]
    fn recent_output_expires_and_new_output_extends_it() {
        let start = Instant::now();
        let timeout = Duration::from_secs(5);
        assert_eq!(remaining_activity(true, None, timeout, start), None);
        assert_eq!(remaining_activity(false, Some(start), timeout, start), None);
        assert_eq!(
            remaining_activity(true, Some(start), timeout, start + Duration::from_secs(4)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            remaining_activity(true, Some(start), timeout, start + timeout),
            None
        );
        assert_eq!(
            remaining_activity(
                true,
                Some(start + Duration::from_secs(4)),
                timeout,
                start + timeout
            ),
            Some(Duration::from_secs(4))
        );
    }
}
