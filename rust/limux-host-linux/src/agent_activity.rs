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
/// the blinking bullet of a pending tool), so a working agent whose terminal
/// has been silent this long is treated as idle. A waiting one only if the
/// user has had it on screen since it asked: answering needs them there, and
/// a lost "needs you" is worse than a stale one.
pub const QUIET_AFTER_MS: u64 = 10_000;

/// The agent name of a plain terminal: one that printed on its own (a build,
/// a server, a long command) without any agent hooks. Its state comes from
/// its output, see [`ActivityStore::track_terminal`].
pub const TERMINAL_AGENT: &str = "terminal";

/// What [`ActivityStore::track_terminal`] saw happen to a plain terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalChange {
    None,
    /// It started printing on its own.
    Started,
    /// It stopped printing: the work ended.
    Finished,
}

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
    /// When the current phase began (Unix ms), for "working for 3m".
    pub since_ms: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Default)]
pub struct ActivityStore {
    surfaces: HashMap<String, SurfaceActivity>,
    /// When each tab's entry was removed (its session ended or the user
    /// closed it), so a hook from before then cannot bring it back.
    removed_at: HashMap<String, u64>,
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
        // A plain terminal's state is a guess from its output; any agent
        // hook replaces it.
        if current
            .is_some_and(|current| event.at_ms < current.at_ms && current.agent != TERMINAL_AGENT)
        {
            return false;
        }
        // A late hook from before the session ended (they race over the
        // socket) must not revive it.
        if current.is_none()
            && self
                .removed_at
                .get(&event.tab_id)
                .is_some_and(|removed| event.at_ms < *removed)
        {
            return false;
        }
        match event.phase {
            None => {
                let same_session =
                    current.is_some_and(|current| match (&current.session_id, &event.session_id) {
                        (Some(current), Some(ended)) => current == ended,
                        _ => true,
                    });
                same_session && self.remove(&event.tab_id, event.at_ms)
            }
            Some(phase) => {
                let detail = event.detail.or_else(|| {
                    current
                        .filter(|current| current.phase == phase)
                        .and_then(|current| current.detail.clone())
                });
                let since_ms = current
                    .filter(|current| current.phase == phase)
                    .map_or(event.at_ms, |current| current.since_ms);
                let next = SurfaceActivity {
                    agent: event.agent,
                    phase,
                    session_id: event.session_id,
                    at_ms: event.at_ms,
                    since_ms,
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
        self.removed_at.retain(|tab_id, _| live(tab_id));
        self.surfaces.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Tabs listed as idle, for closing them all at once.
    pub fn idle_tabs(&self) -> Vec<String> {
        self.surfaces
            .iter()
            .filter(|(_, activity)| activity.phase == AgentPhase::Idle)
            .map(|(tab_id, _)| tab_id.clone())
            .collect()
    }

    /// Follow a terminal that agent hooks do not report on, from its own
    /// output: `working_since` is when its current run of output began, or
    /// `None` while it is quiet. A working terminal stays listed, as idle,
    /// once it stops. Tabs an agent reports on are left alone, and so is a
    /// run of output that began before the tab's entry ended: it is the
    /// agent's last output (its exit screen), or one the user closed.
    pub fn track_terminal(
        &mut self,
        tab_id: &str,
        now_ms: u64,
        working_since: Option<u64>,
    ) -> TerminalChange {
        let current = self.surfaces.get_mut(tab_id);
        if current
            .as_ref()
            .is_some_and(|current| current.agent != TERMINAL_AGENT)
        {
            return TerminalChange::None;
        }
        let ended = self.removed_at.get(tab_id).copied();
        match (current, working_since) {
            (None, Some(since)) if ended.is_some_and(|ended| since < ended) => TerminalChange::None,
            (Some(current), Some(_)) if current.phase == AgentPhase::Working => {
                TerminalChange::None
            }
            (_, Some(since)) => {
                self.surfaces.insert(
                    tab_id.to_string(),
                    SurfaceActivity {
                        agent: TERMINAL_AGENT.to_string(),
                        phase: AgentPhase::Working,
                        session_id: None,
                        at_ms: now_ms,
                        since_ms: since,
                        detail: None,
                    },
                );
                TerminalChange::Started
            }
            (Some(current), None) if current.phase == AgentPhase::Working => {
                current.phase = AgentPhase::Idle;
                current.at_ms = now_ms;
                current.since_ms = now_ms;
                TerminalChange::Finished
            }
            _ => TerminalChange::None,
        }
    }

    /// Keep a terminal listed (as idle) that has something for the user but
    /// no state yet, such as an unread notification. Returns whether it was
    /// added.
    pub fn keep(&mut self, tab_id: &str, now_ms: u64) -> bool {
        if self.surfaces.contains_key(tab_id) {
            return false;
        }
        self.surfaces.insert(
            tab_id.to_string(),
            SurfaceActivity {
                agent: TERMINAL_AGENT.to_string(),
                phase: AgentPhase::Idle,
                session_id: None,
                at_ms: now_ms,
                since_ms: now_ms,
                detail: None,
            },
        );
        true
    }

    /// The user closed a listed terminal: drop it until it does something
    /// again. Only an idle one: working and waiting say what is going on.
    /// Returns whether it was dropped.
    pub fn dismiss(&mut self, tab_id: &str) -> bool {
        let Some(at_ms) = self
            .get(tab_id)
            .filter(|activity| activity.phase == AgentPhase::Idle)
            .map(|activity| activity.at_ms)
        else {
            return false;
        };
        self.remove(tab_id, at_ms)
    }

    /// Drop a tab's entry, remembering when (`at_ms`, or its newest event if
    /// later) so older events stay rejected.
    fn remove(&mut self, tab_id: &str, at_ms: u64) -> bool {
        let Some(removed) = self.surfaces.remove(tab_id) else {
            return false;
        };
        self.removed_at
            .insert(tab_id.to_string(), at_ms.max(removed.at_ms));
        true
    }

    /// Settle working or waiting tabs that went quiet: no hook for
    /// [`QUIET_AFTER_MS`] and no terminal output for as long. `silent_ms`
    /// gives how long a tab's terminal has had no output, `None` when output
    /// is not tracked (then the tab is left alone). `seen_after(tab, at_ms)`
    /// says whether the tab was on screen after `at_ms`; a waiting tab settles
    /// only then. Returns whether any changed. A later hook still applies, as
    /// its time is newer.
    pub fn settle_quiet(
        &mut self,
        now_ms: u64,
        mut silent_ms: impl FnMut(&str) -> Option<u64>,
        mut seen_after: impl FnMut(&str, u64) -> bool,
    ) -> bool {
        let mut changed = false;
        for (tab_id, activity) in &mut self.surfaces {
            if activity.phase == AgentPhase::Idle
                || activity.agent == TERMINAL_AGENT
                || now_ms.saturating_sub(activity.at_ms) < QUIET_AFTER_MS
                || (activity.phase.is_waiting() && !seen_after(tab_id, activity.at_ms))
            {
                continue;
            }
            if silent_ms(tab_id).is_some_and(|silent| silent >= QUIET_AFTER_MS) {
                activity.phase = AgentPhase::Idle;
                activity.since_ms = now_ms;
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
    /// Idle after doing something, kept listed until closed.
    pub quiet: usize,
}

impl ActivityCounts {
    pub fn add_phase(&mut self, phase: Option<AgentPhase>) {
        match phase {
            Some(AgentPhase::Working) => self.working += 1,
            Some(phase) if phase.is_waiting() => self.waiting += 1,
            Some(_) => self.quiet += 1,
            None => {}
        }
    }

    pub fn add(&mut self, other: Self) {
        self.working += other.working;
        self.waiting += other.waiting;
        self.quiet += other.quiet;
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
        // Still working: the unread mark shows once it stops.
        Some(AgentPhase::Working) => (GLYPH_WORKING, "working"),
        _ if unread => (GLYPH_UNREAD, "unread"),
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
    /// Listed last and compact: nothing to act on, but one click away.
    Working,
    /// Did something earlier and is quiet now; listed until closed.
    Quiet,
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
        let counts = |working, waiting| ActivityCounts {
            working,
            waiting,
            quiet: 0,
        };
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
                waiting: 1,
                quiet: 1
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
                waiting: 2,
                quiet: 0
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
    fn session_glyphs_put_waiting_over_working_over_unread() {
        assert_eq!(
            session_glyph(Some(AgentPhase::NeedsApproval), true).0,
            GLYPH_WAITING
        );
        // A note that came while it works waits until it stops.
        assert_eq!(
            session_glyph(Some(AgentPhase::Working), true).0,
            GLYPH_WORKING
        );
        assert_eq!(session_glyph(Some(AgentPhase::Idle), true).0, GLYPH_UNREAD);
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
        let seen = |_: &str, _: u64| true;
        assert!(store.settle_quiet(100_000, silent, seen));
        for tab in ["working", "approval", "question", "idle"] {
            assert_eq!(store.phase(tab), Some(AgentPhase::Idle), "{tab}");
        }
        // Still drawing, output not tracked, or a hook less than 10 s ago.
        for tab in ["busy", "untracked", "fresh"] {
            assert_eq!(store.phase(tab), Some(AgentPhase::Working), "{tab}");
        }
        assert!(!store.settle_quiet(100_000, silent, seen));
    }

    #[test]
    fn a_hook_after_settling_still_applies() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 1_000));
        assert!(store.settle_quiet(20_000, |_| Some(15_000), |_, _| true));
        assert!(store.apply(event("t", Some(AgentPhase::Working), Some("s"), 21_000)));
        assert_eq!(store.phase("t"), Some(AgentPhase::Working));
    }

    #[test]
    fn a_quiet_waiting_agent_stays_until_the_user_saw_it() {
        let mut store = ActivityStore::default();
        store.apply(event(
            "asked",
            Some(AgentPhase::NeedsInput),
            Some("s"),
            1_000,
        ));
        store.apply(event(
            "working",
            Some(AgentPhase::Working),
            Some("s"),
            1_000,
        ));
        let silent = |_: &str| Some(QUIET_AFTER_MS);
        // Not on screen since it asked (seen only before): the question stays.
        assert!(store.settle_quiet(20_000, silent, |_, at| at < 500));
        assert_eq!(store.phase("asked"), Some(AgentPhase::NeedsInput));
        assert_eq!(store.phase("working"), Some(AgentPhase::Idle));
        // Seen after it asked, then silent: answered or denied there.
        assert!(store.settle_quiet(20_000, silent, |_, at| at <= 5_000));
        assert_eq!(store.phase("asked"), Some(AgentPhase::Idle));
    }

    #[test]
    fn a_phase_keeps_its_start_time_while_it_lasts() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 1_000));
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 5_000));
        assert_eq!(store.get("t").unwrap().since_ms, 1_000);
        store.apply(event(
            "t",
            Some(AgentPhase::NeedsApproval),
            Some("s"),
            7_000,
        ));
        assert_eq!(store.get("t").unwrap().since_ms, 7_000);
    }

    #[test]
    fn a_plain_terminal_works_while_it_prints_and_stays_listed_after() {
        let mut store = ActivityStore::default();
        assert_eq!(store.track_terminal("t", 1_000, None), TerminalChange::None);
        assert!(store.get("t").is_none(), "a quiet terminal is not listed");
        assert_eq!(
            store.track_terminal("t", 2_000, Some(1_500)),
            TerminalChange::Started
        );
        let activity = store.get("t").unwrap();
        assert_eq!(activity.phase, AgentPhase::Working);
        assert_eq!(activity.since_ms, 1_500);
        assert_eq!(
            store.track_terminal("t", 3_000, Some(1_500)),
            TerminalChange::None
        );
        assert_eq!(
            store.track_terminal("t", 9_000, None),
            TerminalChange::Finished
        );
        assert_eq!(store.phase("t"), Some(AgentPhase::Idle));
        assert_eq!(
            store.track_terminal("t", 10_000, None),
            TerminalChange::None
        );
        // Closing it drops it until it prints again.
        assert!(store.dismiss("t"));
        assert!(store.get("t").is_none());
        assert_eq!(
            store.track_terminal("t", 20_000, Some(19_000)),
            TerminalChange::Started
        );
        assert!(!store.dismiss("t"), "a working terminal cannot be closed");
    }

    #[test]
    fn agent_hooks_win_over_a_guess_from_output() {
        let mut store = ActivityStore::default();
        store.track_terminal("t", 5_000, Some(4_000));
        // A hook that ran just before the guess still applies.
        assert!(store.apply(event("t", Some(AgentPhase::NeedsApproval), None, 4_900)));
        assert_eq!(store.phase("t"), Some(AgentPhase::NeedsApproval));
        // Output no longer moves an agent's state.
        assert_eq!(store.track_terminal("t", 6_000, None), TerminalChange::None);
        assert_eq!(store.phase("t"), Some(AgentPhase::NeedsApproval));
        assert!(!store.dismiss("t"), "a waiting agent cannot be closed");
    }

    #[test]
    fn an_unread_terminal_is_kept_as_idle() {
        let mut store = ActivityStore::default();
        assert!(store.keep("t", 1_000));
        assert!(!store.keep("t", 2_000));
        assert_eq!(store.phase("t"), Some(AgentPhase::Idle));
        store.apply(event("u", Some(AgentPhase::Working), None, 1_000));
        assert!(!store.keep("u", 2_000), "an agent's state is left alone");
        assert_eq!(store.phase("u"), Some(AgentPhase::Working));
    }

    #[test]
    fn an_agent_s_last_output_is_not_the_shell_working() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 1_000));
        // The user quits the agent: its session ends at 5_000, while the run
        // of output it drew since 2_000 (and its exit screen) still counts.
        assert!(store.apply(event("t", None, Some("s"), 5_000)));
        assert_eq!(
            store.track_terminal("t", 6_000, Some(2_000)),
            TerminalChange::None
        );
        assert!(
            store.get("t").is_none(),
            "not listed for the agent's output"
        );
        // Output the shell makes afterwards is its own.
        assert_eq!(
            store.track_terminal("t", 20_000, Some(19_000)),
            TerminalChange::Started
        );
    }

    #[test]
    fn a_late_hook_from_before_the_end_does_not_revive_the_agent() {
        let mut store = ActivityStore::default();
        store.apply(event("t", Some(AgentPhase::Working), Some("s"), 1_000));
        assert!(store.apply(event("t", None, Some("s"), 3_000)));
        assert!(store.get("t").is_none());
        // A PostToolUse sent at 2_000 arrives after the end.
        assert!(!store.apply(event("t", Some(AgentPhase::Working), Some("s"), 2_000)));
        assert!(store.get("t").is_none());
        // A new session afterwards still starts.
        assert!(store.apply(event("t", Some(AgentPhase::Working), Some("n"), 4_000)));
        assert_eq!(store.phase("t"), Some(AgentPhase::Working));
        // Closing the tab forgets it, so its id carries nothing over.
        store.retain_tabs(|_| false);
        assert!(store.apply(event("t", Some(AgentPhase::Working), None, 10)));
    }
}
