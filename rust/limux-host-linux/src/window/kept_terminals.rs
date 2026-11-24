//! The terminals the sidebar keeps listed (until closed), saved next to the
//! session so a restart does not empty the lists. They come back quiet (`○`)
//! with their last line and message: what an agent was doing before the
//! restart is not claimed until it reports again.

use super::*;

const FILE_NAME: &str = "kept-terminals.json";

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub(super) struct KeptTerminal {
    pub tab_id: String,
    pub agent: String,
    pub since_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The last message shown under it, and when it came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<(String, u64)>,
}

fn file_path() -> std::path::PathBuf {
    crate::layout_state::persistence_dir().join(FILE_NAME)
}

/// What to save: every listed terminal, oldest first so the file is stable.
pub(super) fn collect(s: &AppState) -> Vec<KeptTerminal> {
    let mut kept: Vec<KeptTerminal> = s
        .agent_activity
        .entries()
        .map(|(tab_id, activity)| KeptTerminal {
            tab_id: tab_id.clone(),
            agent: activity.agent.clone(),
            since_ms: activity.since_ms,
            detail: activity.detail.clone(),
            message: s.tab_notes.get(tab_id).cloned(),
        })
        .collect();
    kept.sort_by(|a, b| (a.since_ms, &a.tab_id).cmp(&(b.since_ms, &b.tab_id)));
    kept
}

/// At most this many entries in the file, the newest kept.
const MAX_KEPT: usize = 256;

fn read_file() -> Vec<KeptTerminal> {
    std::fs::read(file_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// This window's kept terminals merged into the file: entries of tabs it
/// owns (open here, or written by it before) are replaced, the rest (another
/// window's) stay. Newest first past [`MAX_KEPT`].
pub(super) fn merge(
    ours: &[KeptTerminal],
    file: Vec<KeptTerminal>,
    owned: impl Fn(&str) -> bool,
) -> Vec<KeptTerminal> {
    let mut merged: Vec<KeptTerminal> = ours.to_vec();
    merged.extend(
        file.into_iter().filter(|kept| {
            !owned(&kept.tab_id) && !ours.iter().any(|our| our.tab_id == kept.tab_id)
        }),
    );
    merged.sort_by(|a, b| (b.since_ms, &b.tab_id).cmp(&(a.since_ms, &a.tab_id)));
    merged.truncate(MAX_KEPT);
    merged.reverse();
    merged
}

/// Save the kept terminals with the session; best effort, like the rest of
/// it, and only when they changed.
pub(super) fn save(state: &State) {
    let ours = collect(&state.borrow());
    if state.borrow().kept_written.as_ref() == Some(&ours) {
        return;
    }
    let live = pane::terminal_tabs_by_id();
    let written: Vec<String> = state
        .borrow()
        .kept_written
        .iter()
        .flatten()
        .map(|kept| kept.tab_id.clone())
        .collect();
    let merged = merge(&ours, read_file(), |tab_id| {
        live.contains_key(tab_id) || written.iter().any(|id| id == tab_id)
    });
    let Ok(json) = serde_json::to_vec(&merged) else {
        return;
    };
    let path = file_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&temp, json).is_ok() && std::fs::rename(&temp, &path).is_ok() {
        state.borrow_mut().kept_written = Some(ours);
    }
}

/// Bring the kept terminals back for the tabs the restored session still
/// has (alive, not exited). Returns how many came back.
pub(super) fn restore(state: &State) -> usize {
    let kept = read_file();
    let live = pane::terminal_tabs_by_id();
    let mut restored = 0;
    {
        let mut s = state.borrow_mut();
        for terminal in kept {
            if live.get(&terminal.tab_id) != Some(&false) {
                continue;
            }
            if s.agent_activity.restore_quiet(
                &terminal.tab_id,
                &terminal.agent,
                terminal.since_ms,
                terminal.detail,
            ) {
                restored += 1;
                if let Some(message) = terminal.message {
                    s.tab_notes.insert(terminal.tab_id, message);
                }
            }
        }
    }
    if restored > 0 {
        agent_activity_ui::refresh_activity(state);
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(tab_id: &str, since_ms: u64) -> KeptTerminal {
        KeptTerminal {
            tab_id: tab_id.to_string(),
            agent: "claude".to_string(),
            since_ms,
            detail: None,
            message: None,
        }
    }

    #[test]
    fn a_save_keeps_the_other_windows_terminals() {
        // The file holds this window's old entry (a), one it closed since (b),
        // and another window's (c).
        let file = vec![kept("a", 1), kept("b", 2), kept("c", 3)];
        let ours = vec![kept("a", 5), kept("d", 6)];
        let owned = |tab_id: &str| matches!(tab_id, "a" | "b" | "d");
        let merged = merge(&ours, file, owned);
        let ids: Vec<&str> = merged.iter().map(|kept| kept.tab_id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "d"], "oldest first, b dropped, c kept");
        assert_eq!(merged[1].since_ms, 5, "this window's entry wins");
    }

    #[test]
    fn the_file_keeps_only_the_newest() {
        let file: Vec<KeptTerminal> = (0..MAX_KEPT as u64 + 10)
            .map(|n| kept(&format!("other-{n}"), n))
            .collect();
        let merged = merge(&[kept("ours", u64::MAX)], file, |_| false);
        assert_eq!(merged.len(), MAX_KEPT);
        assert_eq!(merged.last().unwrap().tab_id, "ours");
    }
}
