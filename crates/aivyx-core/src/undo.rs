//! The `/undo` model: which turns changed files and where to rewind each
//! one to. Pure — the agent does the git work (see `agent/mod.rs`).

use serde::{Deserialize, Serialize};

/// Matches `aivyx-checkpoint`'s retention (it keeps the newest 50 refs).
pub const LEDGER_CAP: usize = 50;

/// One turn that changed files: the checkpoint taken just before its first
/// change, and (once the turn ends) a snapshot of how it left things.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMark {
    pub user_text_preview: String,
    pub before_ref: String,
    pub before_oid: String,
    #[serde(default)]
    pub after_oid: Option<String>,
    pub created_unix: i64,
}

/// An undone turn and the snapshot taken just before undoing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedoMark {
    pub mark: TurnMark,
    pub redo_oid: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoLedger {
    #[serde(default)]
    pub marks: Vec<TurnMark>,
    #[serde(default)]
    pub redo: Vec<RedoMark>,
}

impl UndoLedger {
    /// A new turn changed files: it becomes the newest undoable turn, and
    /// anything that was undone can no longer be redone.
    pub fn record(&mut self, mark: TurnMark) {
        self.redo.clear();
        self.marks.push(mark);
        if self.marks.len() > LEDGER_CAP {
            let excess = self.marks.len() - LEDGER_CAP;
            self.marks.drain(..excess);
        }
    }

    pub fn set_last_after(&mut self, after_oid: String) {
        if let Some(last) = self.marks.last_mut()
            && last.after_oid.is_none()
        {
            last.after_oid = Some(after_oid);
        }
    }

    pub fn pop_mark(&mut self) -> Option<TurnMark> {
        self.marks.pop()
    }

    pub fn push_mark_back(&mut self, mark: TurnMark) {
        self.marks.push(mark);
    }

    pub fn push_redo(&mut self, redo: RedoMark) {
        self.redo.push(redo);
    }

    pub fn pop_redo(&mut self) -> Option<RedoMark> {
        self.redo.pop()
    }

    pub fn clear(&mut self) {
        self.marks.clear();
        self.redo.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Removed,
    Restored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEntry {
    pub path: String,
    pub kind: ChangeKind,
    pub changed_after: bool,
}

/// `git diff --name-status <current> <target>` → what restoring `target`
/// does to each path.
pub fn parse_name_status(from_to_name_status: &str) -> Vec<(String, ChangeKind)> {
    let mut out = Vec::new();
    for line in from_to_name_status.lines() {
        let mut parts = line.split('\t');
        let Some(status) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        match status.chars().next() {
            Some('M') | Some('T') => {
                if let Some(p) = parts.next() {
                    out.push((p.to_string(), ChangeKind::Modified));
                }
            }
            Some('D') => {
                if let Some(p) = parts.next() {
                    out.push((p.to_string(), ChangeKind::Removed));
                }
            }
            Some('A') => {
                if let Some(p) = parts.next() {
                    out.push((p.to_string(), ChangeKind::Restored));
                }
            }
            Some('R') => {
                if let (Some(old), Some(new)) = (parts.next(), parts.next()) {
                    out.push((old.to_string(), ChangeKind::Restored));
                    out.push((new.to_string(), ChangeKind::Removed));
                }
            }
            _ => {}
        }
    }
    out
}

/// The `/undo` / `/redo` confirmation text. The git-ignored caveat is
/// always shown: checkpoint trees never contain ignored files, so whether
/// one is affected can't be told from them.
pub fn preview_text(title: &str, entries: &[PreviewEntry]) -> String {
    let mut text = format!("{title}\n");
    for e in entries {
        let (sym, note) = match e.kind {
            ChangeKind::Modified => ("~", ""),
            ChangeKind::Removed => ("−", "   (will be removed)"),
            ChangeKind::Restored => ("+", "   (will come back)"),
        };
        text.push_str(&format!("\n{sym} {}{note}", e.path));
        if e.changed_after {
            text.push_str("   ⚠ changed after the turn");
        }
    }
    text.push_str("\n\n(git-ignored files are not touched)");
    text
}

pub fn preview_title_undo(user_text_preview: &str) -> String {
    format!("Undo the last turn (\"{user_text_preview}\")?")
}

pub fn text_preview(user_text: &str) -> String {
    let one_line = user_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 60 {
        format!("{}…", one_line.chars().take(60).collect::<String>())
    } else {
        one_line
    }
}

pub fn checkpoints_listing(
    marks: &[TurnMark],
    _now_unix: i64,
    local_offset_secs: i32,
    is_pruned: impl Fn(&TurnMark) -> bool,
) -> String {
    if marks.is_empty() {
        return "Nothing to undo — no changes made in this session.".to_string();
    }
    let mut text = "Undoable turns (newest first — /undo takes back the top one):".to_string();
    for (i, m) in marks.iter().rev().enumerate() {
        let local = m.created_unix + i64::from(local_offset_secs);
        let (h, min) = ((local.rem_euclid(86_400)) / 3_600, (local.rem_euclid(3_600)) / 60);
        text.push_str(&format!("\n{}  {h:02}:{min:02} · \"{}\"", i + 1, m.user_text_preview));
        if is_pruned(m) {
            text.push_str("   too old to undo");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(n: i64) -> TurnMark {
        TurnMark {
            user_text_preview: format!("turn {n}"),
            before_ref: format!("refs/aivyx/checkpoints/{n}"),
            before_oid: format!("{n:040x}"),
            after_oid: None,
            created_unix: n,
        }
    }

    #[test]
    fn record_clears_redo_and_caps() {
        let mut l = UndoLedger::default();
        l.push_redo(RedoMark { mark: mark(0), redo_oid: "r".into() });
        for n in 0..(LEDGER_CAP as i64 + 5) {
            l.record(mark(n));
        }
        assert!(l.redo.is_empty());
        assert_eq!(l.marks.len(), LEDGER_CAP);
        assert_eq!(l.marks[0].created_unix, 5, "oldest dropped first");
    }

    #[test]
    fn undo_redo_stack_round_trip() {
        let mut l = UndoLedger::default();
        l.record(mark(1));
        l.set_last_after("a".repeat(40));
        assert_eq!(l.marks[0].after_oid.as_deref(), Some("a".repeat(40).as_str()));
        let m = l.pop_mark().unwrap();
        l.push_redo(RedoMark { mark: m.clone(), redo_oid: "b".repeat(40) });
        assert!(l.pop_mark().is_none());
        let r = l.pop_redo().unwrap();
        l.push_mark_back(r.mark.clone());
        assert_eq!(l.marks, vec![m]);
        assert!(l.redo.is_empty());
    }

    #[test]
    fn serde_round_trip_and_missing_fields_default() {
        let mut l = UndoLedger::default();
        l.record(mark(3));
        let json = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<UndoLedger>(&json).unwrap(), l);
        assert_eq!(serde_json::from_str::<UndoLedger>("{}").unwrap(), UndoLedger::default());
    }

    #[test]
    fn name_status_maps_to_restore_effects() {
        let out = "M\tsrc/a.rs\nD\tnew.txt\nA\tgone.txt\nR100\told.rs\tnew.rs\n\n";
        assert_eq!(
            parse_name_status(out),
            vec![
                ("src/a.rs".to_string(), ChangeKind::Modified),
                ("new.txt".to_string(), ChangeKind::Removed),
                ("gone.txt".to_string(), ChangeKind::Restored),
                ("old.rs".to_string(), ChangeKind::Restored),
                ("new.rs".to_string(), ChangeKind::Removed),
            ]
        );
    }

    #[test]
    fn name_status_skips_copy_status_as_unknown() {
        // A copy's source exists in both trees, so treating it like a
        // rename would wrongly claim it "will come back".
        let out = "C100\told\tnew";
        assert_eq!(parse_name_status(out), vec![]);
    }

    #[test]
    fn preview_lists_symbols_flags_and_ignored_note() {
        let entries = vec![
            PreviewEntry { path: "stats.py".into(), kind: ChangeKind::Modified, changed_after: true },
            PreviewEntry { path: "notes.md".into(), kind: ChangeKind::Removed, changed_after: false },
            PreviewEntry { path: "old.txt".into(), kind: ChangeKind::Restored, changed_after: false },
            PreviewEntry { path: "draft.md".into(), kind: ChangeKind::Removed, changed_after: true },
            PreviewEntry { path: "gone.rs".into(), kind: ChangeKind::Restored, changed_after: true },
        ];
        let text = preview_text("Undo the last turn (\"fix it\")?", &entries);
        assert_eq!(
            text,
            "Undo the last turn (\"fix it\")?\n\n\
             ~ stats.py   ⚠ changed after the turn\n\
             − notes.md   (will be removed)\n\
             + old.txt   (will come back)\n\
             − draft.md   (will be removed)   ⚠ changed after the turn\n\
             + gone.rs   (will come back)   ⚠ changed after the turn\n\n\
             (git-ignored files are not touched)"
        );
        // The ignored-files caveat is always shown: checkpoints never
        // contain ignored files, so there is no way to tell from them.
        let plain = preview_text("t", &entries[1..2]);
        assert_eq!(plain, "t\n\n− notes.md   (will be removed)\n\n(git-ignored files are not touched)");
    }

    #[test]
    fn text_preview_is_one_line_and_bounded() {
        assert_eq!(text_preview("fix\nthe tests"), "fix the tests");
        let long = "x".repeat(80);
        assert_eq!(text_preview(&long), format!("{}…", "x".repeat(60)));
    }

    #[test]
    fn checkpoints_listing_newest_first_with_pruned_marker() {
        let mut a = mark(1_000);
        a.user_text_preview = "first".into();
        let mut b = mark(1_000 + 3_600);
        b.user_text_preview = "second".into();
        let text = checkpoints_listing(&[a.clone(), b], 1_000 + 7_200, 0, |m| m.created_unix == a.created_unix);
        assert_eq!(
            text,
            "Undoable turns (newest first — /undo takes back the top one):\n\
             1  01:16 · \"second\"\n\
             2  00:16 · \"first\"   too old to undo"
        );
        assert_eq!(
            checkpoints_listing(&[], 0, 0, |_| false),
            "Nothing to undo — no changes made in this session."
        );
    }
}
