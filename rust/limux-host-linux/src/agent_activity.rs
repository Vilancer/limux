//! Live agent state per terminal, reported by agent hooks through the
//! `agent.activity` control method, and the summaries the sidebar draws
//! from it. Pure: no GTK, so the rules are table-tested.
//!
//! The state is kept per terminal tab (its tab id), separate from unread and
//! from recent PTY output. Unread stays where it was (tab and workspace
//! flags); "response ready" is the unread mark the Stop notification already
//! sets. Viewing a terminal never changes this state: only a hook does.

use std::collections::HashMap;

/// No hook reports an interrupted turn (Esc), a denied permission or a killed
/// agent. While Claude Code works or waits it keeps redrawing (spinner, timer,
/// the blinking bullet of a pending tool), so a working or waiting agent whose
/// terminal has been silent this long is treated as idle.
pub const QUIET_AFTER_MS: u64 = 10_000;

/// What an agent in a terminal is doing, as its hooks last said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentPhase {
    /// A turn is running (prompt submitted, or a tool ran).
    Working,
    /// The agent asked for permission to use a tool.
    NeedsApproval,
    /// The agent asked a question (an input dialog).
    NeedsInput,
    /// The agent is waiting for the next prompt.
    Idle,
}

impl AgentPhase {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "working" => Some(Self::Working),
            "needs_approval" => Some(Self::NeedsApproval),
            "needs_input" => Some(Self::NeedsInput),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }

    pub fn is_waiting(self) -> bool {
        matches!(self, Self::NeedsApproval | Self::NeedsInput)
    }
}

/// One normalized hook event for a terminal tab. `phase: None` means the
/// agent session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEvent {
    pub tab_id: String,
    pub agent: String,
    pub phase: Option<AgentPhase>,
    pub session_id: Option<String>,
    /// When the hook ran (Unix ms). Orders events that race over the socket.
    pub at_ms: u64,
    /// What the agent does, in one line ("Bash: cargo test"); `None` keeps
    /// the previous line while the phase stays the same.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceActivity {
    pub agent: String,
    pub phase: AgentPhase,
    pub session_id: Option<String>,
    pub at_ms: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Default)]
pub struct ActivityStore {
    surfaces: HashMap<String, SurfaceActivity>,
}

impl ActivityStore {
    /// Apply an event; returns whether anything changed.
    ///
    /// - An event older than the tab's last one is dropped, so a late hook
    ///   from an earlier turn cannot clear a newer state.
    /// - The end of a session only clears the tab when it is that session
    ///   (or either side does not know its session).
    pub fn apply(&mut self, event: AgentEvent) -> bool {
        let current = self.surfaces.get(&event.tab_id);
        if current.is_some_and(|current| event.at_ms < current.at_ms) {
            return false;
        }
        match event.phase {
            None => {
                let same_session =
                    current.is_some_and(|current| match (&current.session_id, &event.session_id) {
                        (Some(current), Some(ended)) => current == ended,
                        _ => true,
                    });
                same_session && self.surfaces.remove(&event.tab_id).is_some()
            }
            Some(phase) => {
                let detail = event.detail.or_else(|| {
                    current
                        .filter(|current| current.phase == phase)
                        .and_then(|current| current.detail.clone())
                });
                let next = SurfaceActivity {
                    agent: event.agent,
                    phase,
                    session_id: event.session_id,
                    at_ms: event.at_ms,
                    detail,
                };
                let changed = current.is_none_or(|current| {
                    current.phase != next.phase
                        || current.agent != next.agent
                        || current.detail != next.detail
                });
                self.surfaces.insert(event.tab_id, next);
                changed
            }
        }
    }

    pub fn get(&self, tab_id: &str) -> Option<&SurfaceActivity> {
        self.surfaces.get(tab_id)
    }

    pub fn phase(&self, tab_id: &str) -> Option<AgentPhase> {
        self.get(tab_id).map(|activity| activity.phase)
    }

    /// Forget tabs that no longer exist, so a closed or exited terminal
    /// drops out of every count. Returns whether anything was removed.
    pub fn retain_tabs(&mut self, mut live: impl FnMut(&str) -> bool) -> bool {
        let before = self.surfaces.len();
        self.surfaces.retain(|tab_id, _| live(tab_id));
        self.surfaces.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Settle working or waiting tabs that went quiet: no hook for
    /// [`QUIET_AFTER_MS`] and no terminal output for as long. `silent_ms`
    /// gives how long a tab's terminal has had no output, `None` when output
    /// is not tracked (then the tab is left alone). Returns whether any
    /// changed. A later hook still applies, as its time is newer.
    pub fn settle_quiet(
        &mut self,
        now_ms: u64,
        mut silent_ms: impl FnMut(&str) -> Option<u64>,
    ) -> bool {
        let mut changed = false;
        for (tab_id, activity) in &mut self.surfaces {
            if activity.phase == AgentPhase::Idle
                || now_ms.saturating_sub(activity.at_ms) < QUIET_AFTER_MS
            {
                continue;
            }
            if silent_ms(tab_id).is_some_and(|silent| silent >= QUIET_AFTER_MS) {
                activity.phase = AgentPhase::Idle;
                activity.detail = None;
                changed = true;
            }
        }
        changed
    }
}

/// Working and waiting terminals in a project or group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityCounts {
    pub working: usize,
    pub waiting: usize,
}

impl ActivityCounts {
    pub fn add_phase(&mut self, phase: Option<AgentPhase>) {
        match phase {
            Some(AgentPhase::Working) => self.working += 1,
            Some(phase) if phase.is_waiting() => self.waiting += 1,
            _ => {}
        }
    }

    pub fn add(&mut self, other: Self) {
        self.working += other.working;
        self.waiting += other.waiting;
    }
}

/// The one signal a project row (and its top-bar pill) shows:
/// waiting over unread over working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSignal {
    None,
    Working,
    Unread,
    Waiting,
}

impl ProjectSignal {
    pub fn of(counts: ActivityCounts, unread: bool) -> Self {
        if counts.waiting > 0 {
            Self::Waiting
        } else if unread {
            Self::Unread
        } else if counts.working > 0 {
            Self::Working
        } else {
            Self::None
        }
    }

    /// The pill shows "needs attention" only: waiting, else unread.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Waiting | Self::Unread)
    }
}

pub const GLYPH_WAITING: &str = "\u{25C6}"; // ◆
pub const GLYPH_UNREAD: &str = "\u{25CF}"; // ●
pub const GLYPH_WORKING: &str = "\u{21BB}"; // ↻ (static: the tab spinner means recent output)
pub const GLYPH_IDLE: &str = "\u{25CB}"; // ○
pub const GLYPH_SHELL: &str = "\u{203A}"; // ›

/// Glyph and spoken label of one terminal in a session list.
pub fn session_glyph(phase: Option<AgentPhase>, unread: bool) -> (&'static str, &'static str) {
    match phase {
        Some(AgentPhase::NeedsApproval) => (GLYPH_WAITING, "needs approval"),
        Some(AgentPhase::NeedsInput) => (GLYPH_WAITING, "needs input"),
        _ if unread => (GLYPH_UNREAD, "unread"),
        Some(AgentPhase::Working) => (GLYPH_WORKING, "working"),
        Some(AgentPhase::Idle) => (GLYPH_IDLE, "waiting for a prompt"),
        None => (GLYPH_SHELL, "terminal"),
    }
}

/// "3 working, 1 needs you" for tooltips and accessible labels.
pub fn counts_summary(counts: ActivityCounts) -> Option<String> {
    let mut parts = Vec::new();
    if counts.waiting > 0 {
        parts.push(format!(
            "{} need{} you",
            counts.waiting,
            if counts.waiting == 1 { "s" } else { "" }
        ));
    }
    if counts.working > 0 {
        parts.push(format!("{} working", counts.working));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// What a pending-panel entry is: an agent waiting for the user, or an
/// unread terminal (a finished reply or a notification).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PendingKind {
    Waiting,
    Unread,
}

/// Order pending entries `(kind, at_ms, item)`: waiting first, then unread,
/// newest first within each; ties keep their sidebar order.
pub fn sort_pending<T>(entries: &mut [(PendingKind, u64, T)]) {
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
}

/// "now", "5m", "3h", "2d": how long ago `at_ms` was; empty when unknown.
pub fn ago_label(now_ms: u64, at_ms: u64) -> String {
    if at_ms == 0 {
        return String::new();
    }
    let minutes = now_ms.saturating_sub(at_ms) / 60_000;
    match minutes {
        0 => "now".to_string(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// Now, in Unix ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        tab: &str,
        phase: Option<AgentPhase>,
        session: Option<&str>,
        at_ms: u64,
    ) -> AgentEvent {
        AgentEvent {
            tab_id: tab.to_string(),
            agent: "claude".to_string(),
            phase,
            session_id: session.map(str::to_string),
            at_ms,
            detail: None,
        }
    }

    #[test]
    fn phases_parse_from_the_wire_names() {
        for (wire, phase) in [
            ("working", AgentPhase::Working),
            ("needs_approval", AgentPhase::NeedsApproval),
            ("needs_input", AgentPhase::NeedsInput),
            ("idle", AgentPhase::Idle),
        ] {
            assert_eq!(AgentPhase::parse(wire), Some(phase));
        }
        assert_eq!(AgentPhase::parse("ended"), None);
        assert_eq!(AgentPhase::parse("Working"), None);
    }

    #[test]
    fn a_turn_runs_waits_resumes_and_ends() {
        use AgentPhase::*;
        let mut store = ActivityStore::default();
        let steps = [
            (Some(Idle), 1, true),           // session start
            (Some(Working), 2, true),        // prompt submitted
            (Some(NeedsApproval), 3, true),  // permission prompt
            (Some(NeedsApproval), 3, false), // the same hook twice
            (Some(Working), 4, true),        // tool ran after approval
            (Some(Idle), 5, true),           // stop: response ready
        ];
        for (phase, at, changed) in steps {
            assert_eq!(
                store.apply(event("t", phase, Some("s"), at)),
                changed,
                "{phase:?}@{at}"
            );
            assert_eq!(store.phase("t"), phase);
        }
        assert!(store.apply(event("t", None, Some("s"), 6)));
        assert_eq!(store.phase("t"), None);
    }

    #[test]
    fn the_detail_line_follows_what_the_agent_does() {
        let mut store = ActivityStore::default();
        let with = |phase, at, detail: Option<&str>| AgentEvent {
            detail: detail.map(str::to_string),
            ..event("t", Some(phase), Some("s"), at)
        };
        assert!(store.apply(with(AgentPhase::Working, 1, Some("fix the tabs"))));
        assert!(store.apply(with(AgentPhase::Working, 2, Some("Bash: cargo test"))));
        let detail = |store: &ActivityStore| store.get("t").unwrap().detail.clone();
        assert_eq!(detail(&store).as_deref(), Some("Bash: cargo test"));
        // No new line in the same phase keeps the last one.
        assert!(!store.apply(with(AgentPhase::Working, 3, None)));
        assert_eq!(detail(&store).as_deref(), Some("Bash: cargo test"));
        // A new phase without a line drops the old one.
        assert!(store.apply(with(AgentPhase::Idle, 4, None)));
        assert_eq!(detail(&store), None);
    }

    #[test]
    fn a_late_event_never_overwrites_a_newer_one() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 20));
        // The stop of the previous turn arrives after the new prompt.
        assert!(!store.apply(event("t", Some(AgentPhase::Idle), Some("s"), 10)));
        assert!(!store.apply(event("t", None, Some("s"), 10)));
        assert_eq!(store.phase("t"), Some(AgentPhase::Working));
    }

    #[test]
    fn the_end_of_another_session_does_not_clear_the_tab() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("new"), 5));
        assert!(!store.apply(event("t", None, Some("old"), 6)));
        assert_eq!(store.phase("t"), Some(AgentPhase::Working));
        // Unknown on either side: the end applies.
        assert!(store.apply(event("t", None, None, 7)));
        assert!(store.is_empty());
    }

    #[test]
    fn closed_tabs_drop_out() {
        let mut store = ActivityStore::default();
        store.apply(event("a", Some(AgentPhase::Working), None, 1));
        store.apply(event("b", Some(AgentPhase::NeedsInput), None, 1));
        assert!(store.retain_tabs(|tab| tab == "b"));
        assert_eq!(store.phase("a"), None);
        assert_eq!(store.phase("b"), Some(AgentPhase::NeedsInput));
        assert!(!store.retain_tabs(|tab| tab == "b"));
    }

    #[test]
    fn project_signal_prefers_waiting_then_unread_then_working() {
        let counts = |working, waiting| ActivityCounts { working, waiting };
        let table = [
            (counts(0, 0), false, ProjectSignal::None),
            (counts(2, 0), false, ProjectSignal::Working),
            (counts(2, 0), true, ProjectSignal::Unread),
            (counts(0, 0), true, ProjectSignal::Unread),
            (counts(1, 1), true, ProjectSignal::Waiting),
            (counts(0, 3), false, ProjectSignal::Waiting),
        ];
        for (counts, unread, signal) in table {
            assert_eq!(
                ProjectSignal::of(counts, unread),
                signal,
                "{counts:?} {unread}"
            );
        }
        assert!(ProjectSignal::Waiting.needs_attention());
        assert!(ProjectSignal::Unread.needs_attention());
        assert!(!ProjectSignal::Working.needs_attention());
    }

    #[test]
    fn counts_add_up_and_read_out() {
        let mut counts = ActivityCounts::default();
        for phase in [
            Some(AgentPhase::Working),
            Some(AgentPhase::Working),
            Some(AgentPhase::NeedsApproval),
            Some(AgentPhase::Idle),
            None,
        ] {
            counts.add_phase(phase);
        }
        assert_eq!(
            counts,
            ActivityCounts {
                working: 2,
                waiting: 1
            }
        );
        assert_eq!(
            counts_summary(counts).as_deref(),
            Some("1 needs you, 2 working")
        );
        assert_eq!(counts_summary(ActivityCounts::default()), None);
        assert_eq!(
            counts_summary(ActivityCounts {
                working: 0,
                waiting: 2
            })
            .as_deref(),
            Some("2 need you")
        );
    }

    #[test]
    fn pending_entries_put_waiting_first_then_newest() {
        let mut entries = vec![
            (PendingKind::Unread, 10, "old reply"),
            (PendingKind::Waiting, 5, "old question"),
            (PendingKind::Unread, 30, "new reply"),
            (PendingKind::Waiting, 20, "new question"),
            (PendingKind::Unread, 30, "same time, later in sidebar"),
        ];
        sort_pending(&mut entries);
        let order: Vec<&str> = entries.iter().map(|(_, _, item)| *item).collect();
        assert_eq!(
            order,
            [
                "new question",
                "old question",
                "new reply",
                "same time, later in sidebar",
                "old reply"
            ]
        );
    }

    #[test]
    fn ago_labels_round_down() {
        let now = 10 * 24 * 3_600_000;
        assert_eq!(ago_label(now, 0), "");
        assert_eq!(ago_label(now, now - 30_000), "now");
        assert_eq!(ago_label(now, now - 5 * 60_000), "5m");
        assert_eq!(ago_label(now, now - 3 * 3_600_000 - 1), "3h");
        assert_eq!(ago_label(now, now - 2 * 86_400_000), "2d");
        // A clock that went backwards reads as now.
        assert_eq!(ago_label(now, now + 60_000), "now");
    }

    #[test]
    fn session_glyphs_put_waiting_over_unread_over_working() {
        assert_eq!(
            session_glyph(Some(AgentPhase::NeedsApproval), true).0,
            GLYPH_WAITING
        );
        assert_eq!(
            session_glyph(Some(AgentPhase::Working), true).0,
            GLYPH_UNREAD
        );
        assert_eq!(
            session_glyph(Some(AgentPhase::Working), false).0,
            GLYPH_WORKING
        );
        assert_eq!(session_glyph(None, false).0, GLYPH_SHELL);
        assert_eq!(
            session_glyph(Some(AgentPhase::Idle), false).1,
            "waiting for a prompt"
        );
    }

    #[test]
    fn quiet_working_or_waiting_agent_settles_to_idle() {
        let mut store = ActivityStore::default();
        for (tab, phase) in [
            ("working", AgentPhase::Working),
            ("approval", AgentPhase::NeedsApproval),
            ("question", AgentPhase::NeedsInput),
            ("idle", AgentPhase::Idle),
            ("busy", AgentPhase::Working),
            ("untracked", AgentPhase::Working),
            ("fresh", AgentPhase::Working),
        ] {
            let at = if tab == "fresh" { 95_000 } else { 1_000 };
            store.apply(event(tab, Some(phase), Some("s"), at));
        }
        let silent = |tab: &str| match tab {
            "busy" => Some(300),
            "untracked" => None,
            _ => Some(QUIET_AFTER_MS),
        };
        assert!(store.settle_quiet(100_000, silent));
        for tab in ["working", "approval", "question", "idle"] {
            assert_eq!(store.phase(tab), Some(AgentPhase::Idle), "{tab}");
        }
        // Still drawing, output not tracked, or a hook less than 10 s ago.
        for tab in ["busy", "untracked", "fresh"] {
            assert_eq!(store.phase(tab), Some(AgentPhase::Working), "{tab}");
        }
        assert!(!store.settle_quiet(100_000, silent));
    }

    #[test]
    fn a_hook_after_settling_still_applies() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 1_000));
        assert!(store.settle_quiet(20_000, |_| Some(15_000)));
        assert!(store.apply(event("t", Some(AgentPhase::Working), Some("s"), 21_000)));
        assert_eq!(store.phase("t"), Some(AgentPhase::Working));
    }
}
