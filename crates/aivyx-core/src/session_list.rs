//! Pure formatting for `/sessions`: `when_label` (a local-time
//! "today HH:MM"/"yesterday HH:MM"/"Mon 29 Sep HH:MM" label) and
//! `sessions_listing` (the full listing text). No I/O and no `Agent` here
//! -- see `crate::agent::session_commands` for the command that calls
//! these against a real `SessionStore`.

use chrono::Datelike;

use crate::session::SessionMeta;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A local-time label for `ts` (Unix seconds), relative to `now` (also Unix
/// seconds) -- both shifted by the same `offset_secs` (local-minus-UTC, as
/// `chrono::Local::now().offset().local_minus_utc()` gives it) before
/// comparing calendar days, so "today"/"yesterday" are judged against the
/// *local* day boundary, not UTC's. `chrono::DateTime::from_timestamp` is
/// used only to read the already-offset value's calendar fields (weekday,
/// day, month) -- it never applies a timezone of its own, since the shift
/// already happened above; `unwrap_or` only ever falls back for a `ts` so
/// far outside representable range it's already meaningless.
pub fn when_label(ts: i64, now: i64, offset_secs: i32) -> String {
    let offset = i64::from(offset_secs);
    let local = ts + offset;
    let now_local = now + offset;
    let day = local.div_euclid(86_400);
    let now_day = now_local.div_euclid(86_400);

    let secs_of_day = local.rem_euclid(86_400);
    let hm = format!(
        "{:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    );

    if day == now_day {
        format!("today {hm}")
    } else if day == now_day - 1 {
        format!("yesterday {hm}")
    } else {
        let dt = chrono::DateTime::from_timestamp(local, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH);
        let weekday = WEEKDAYS[dt.weekday().num_days_from_monday() as usize];
        let month = MONTHS[dt.month0() as usize];
        format!("{weekday} {} {month} {hm}", dt.day())
    }
}

/// The full `/sessions` listing: one row per conversation, in the order
/// given (callers pass `SessionStore::list()`'s own newest-first order --
/// this function does no sorting of its own), the current conversation
/// (matched by id) marked `(current)`. Empty input gets the none-saved
/// string instead of a header with no rows under it.
pub fn sessions_listing(
    metas: &[SessionMeta],
    current_id: Option<&str>,
    now: i64,
    offset_secs: i32,
) -> String {
    if metas.is_empty() {
        return "No saved conversations for this project yet.".to_string();
    }

    let mut text =
        "Conversations for this project (newest first — /resume N to switch):".to_string();
    for (i, meta) in metas.iter().enumerate() {
        let when = when_label(meta.updated_unix, now, offset_secs);
        let turn_word = if meta.turns == 1 { "turn" } else { "turns" };
        text.push_str(&format!(
            "\n{}  {when} · {} {turn_word} · \"{}\"",
            i + 1,
            meta.turns,
            meta.first_user_text
        ));
        if current_id == Some(meta.id.as_str()) {
            text.push_str("  (current)");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    // 2026-10-03 14:02:00 UTC is a Saturday.
    const NOW: i64 = 1_791_036_120;

    #[test]
    fn when_label_today_yesterday_and_older() {
        assert_eq!(when_label(NOW, NOW, 0), "today 14:02");
        assert_eq!(when_label(NOW - 86_400, NOW, 0), "yesterday 14:02");
        assert_eq!(when_label(NOW - 4 * 86_400, NOW, 0), "Tue 29 Sep 14:02");
        // +8 h: 22:02 local, still today.
        assert_eq!(when_label(NOW, NOW, 8 * 3600), "today 22:02");
    }

    #[test]
    fn listing_marks_the_current_one_and_pluralises() {
        let metas = vec![
            SessionMeta {
                id: "b".into(),
                created_unix: NOW,
                updated_unix: NOW,
                first_user_text: "fix it".into(),
                turns: 6,
                revision: 0,
            },
            SessionMeta {
                id: "a".into(),
                created_unix: NOW - 86_400,
                updated_unix: NOW - 86_400,
                first_user_text: "hello".into(),
                turns: 1,
                revision: 0,
            },
        ];
        assert_eq!(
            sessions_listing(&metas, Some("b"), NOW, 0),
            "Conversations for this project (newest first — /resume N to switch):\n\
             1  today 14:02 · 6 turns · \"fix it\"  (current)\n\
             2  yesterday 14:02 · 1 turn · \"hello\""
        );
        assert_eq!(
            sessions_listing(&[], None, NOW, 0),
            "No saved conversations for this project yet."
        );
    }
}
