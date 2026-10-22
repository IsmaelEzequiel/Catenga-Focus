//! Dashboard aggregation: pure functions over per-day facts read from the
//! index (hours per title/tag, done days, streaks, habits). No SQL, no Tauri.

use jiff::ToSpan;
use jiff::civil::Date;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Rows shown in the habit tracker.
const MAX_HABITS: usize = 12;
/// A title is a habit when it was done within this many days...
const HABIT_WINDOW_DAYS: i64 = 90;
/// ...ranked by done days in this many days, which is also the strip length.
const STRIP_DAYS: i64 = 30;
/// The year heatmap: 53 weeks of 7 days.
const CALENDAR_DAYS: i64 = 53 * 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DashRange {
    #[serde(rename = "week")]
    Week,
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "year")]
    Year,
    #[serde(rename = "all")]
    All,
}

/// What happened for one title on one day. Several rows may share a
/// `(date, title_norm)`: session rows carry minutes, note rows carry `has_note`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayFact {
    pub date: String,
    pub title_norm: String,
    pub title: String,
    pub minutes: i64,
    pub sessions: i64,
    pub has_note: bool,
}

/// One tag of one finished session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFact {
    pub date: String,
    /// Empty (with an empty `tag_norm`) for a session that has no tags.
    pub tag: String,
    pub tag_norm: String,
    pub title: String,
    pub title_norm: String,
    pub minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleStat {
    pub label: String,
    pub key: String,
    pub minutes: i64,
    pub sessions: i64,
    pub done_days: i64,
    pub current_streak: i64,
    pub longest_streak: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagStat {
    pub label: String,
    pub key: String,
    pub minutes: i64,
    pub sessions: i64,
    pub titles: Vec<TagTitle>,
}

/// A title's share of one tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagTitle {
    pub label: String,
    pub key: String,
    pub minutes: i64,
    pub sessions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayCell {
    pub date: String,
    pub minutes: i64,
    pub done: bool,
    pub note: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Habit {
    pub label: String,
    pub key: String,
    pub current_streak: i64,
    pub longest_streak: i64,
    pub days: Vec<DayCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub minutes: i64,
    pub sessions: i64,
    pub active_days: i64,
    pub current_streak: i64,
    pub longest_streak: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDay {
    pub date: String,
    pub minutes: i64,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub range: DashRange,
    pub from: Option<String>,
    pub to: String,
    pub totals: Totals,
    pub titles: Vec<TitleStat>,
    pub tags: Vec<TagStat>,
    pub calendar: Vec<CalendarDay>,
    pub habits: Vec<Habit>,
    pub indexing: bool,
}

/// First day of `range` ending at `today`; `None` for `All`. Weeks start on Monday.
pub fn range_start(range: DashRange, today: Date) -> Option<Date> {
    match range {
        DashRange::Week => Some(today - i64::from(today.weekday().to_monday_zero_offset()).days()),
        DashRange::Month => Some(today.first_of_month()),
        DashRange::Days30 => Some(today - 29.days()),
        DashRange::Year => Some(today.first_of_year()),
        DashRange::All => None,
    }
}

/// `(current, longest)` streak of consecutive days in `done`. The current one
/// ends today, or yesterday while today is not done yet (it is still alive).
pub fn streaks(done: &BTreeSet<Date>, today: Date) -> (i64, i64) {
    let mut longest = 0;
    let mut run = 0;
    let mut prev: Option<Date> = None;
    for &d in done.iter().filter(|d| **d <= today) {
        run = if prev.is_some_and(|p| p + 1.day() == d) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        prev = Some(d);
    }
    let mut cursor = if done.contains(&today) {
        today
    } else {
        today - 1.day()
    };
    let mut current = 0;
    while done.contains(&cursor) {
        current += 1;
        cursor -= 1.day();
    }
    (current, longest)
}

/// Per-title working data, all time.
#[derive(Default)]
struct TitleAcc {
    label: String,
    label_date: Option<Date>,
    /// date -> (minutes, sessions, has_note)
    days: BTreeMap<Date, (i64, i64, bool)>,
}

/// Per-tag working data within the range.
#[derive(Default)]
struct TagAcc<'a> {
    label: String,
    label_date: Option<Date>,
    minutes: i64,
    sessions: i64,
    titles: HashMap<&'a str, TagTitleAcc>,
}

#[derive(Default)]
struct TagTitleAcc {
    label: String,
    label_date: Option<Date>,
    minutes: i64,
    sessions: i64,
}

pub fn build(range: DashRange, today: Date, facts: &[DayFact], tag_facts: &[TagFact]) -> Dashboard {
    let from = range_start(range, today);
    let in_range = |d: Date| d <= today && from.is_none_or(|f| d >= f);

    let mut titles: HashMap<&str, TitleAcc> = HashMap::new();
    // Every title, untitled included: date -> (minutes, done).
    let mut overall: BTreeMap<Date, (i64, bool)> = BTreeMap::new();
    let (mut total_minutes, mut total_sessions) = (0, 0);
    for f in facts {
        let Ok(date) = f.date.parse::<Date>() else {
            continue;
        };
        if date > today {
            continue;
        }
        let done = f.sessions > 0 || f.has_note;
        let day = overall.entry(date).or_default();
        day.0 += f.minutes;
        day.1 |= done;
        if in_range(date) {
            total_minutes += f.minutes;
            total_sessions += f.sessions;
        }
        if f.title_norm.is_empty() {
            continue;
        }
        let acc = titles.entry(f.title_norm.as_str()).or_default();
        // The label is the original-case title of the most recent day (last row wins ties).
        if acc.label_date.is_none_or(|d| date >= d) {
            acc.label = f.title.clone();
            acc.label_date = Some(date);
        }
        let slot = acc.days.entry(date).or_default();
        slot.0 += f.minutes;
        slot.1 += f.sessions;
        slot.2 |= f.has_note;
    }

    let done_set = |acc: &TitleAcc| -> BTreeSet<Date> {
        acc.days
            .iter()
            .filter(|(_, (_, s, n))| *s > 0 || *n)
            .map(|(d, _)| *d)
            .collect()
    };

    let mut title_stats = Vec::new();
    for (key, acc) in &titles {
        let done = done_set(acc);
        let (current_streak, longest_streak) = streaks(&done, today);
        let mut stat = TitleStat {
            label: acc.label.clone(),
            key: (*key).to_string(),
            minutes: 0,
            sessions: 0,
            done_days: 0,
            current_streak,
            longest_streak,
        };
        for (d, (m, s, _)) in &acc.days {
            if in_range(*d) {
                stat.minutes += m;
                stat.sessions += s;
            }
        }
        stat.done_days = done.iter().filter(|d| in_range(**d)).count() as i64;
        if stat.minutes > 0 || stat.done_days > 0 {
            title_stats.push(stat);
        }
    }
    title_stats.sort_by(|a, b| {
        b.minutes
            .cmp(&a.minutes)
            .then(b.done_days.cmp(&a.done_days))
            .then_with(|| a.key.cmp(&b.key))
    });

    let mut tag_acc: HashMap<&str, TagAcc> = HashMap::new();
    for t in tag_facts {
        let Ok(date) = t.date.parse::<Date>() else {
            continue;
        };
        if !in_range(date) {
            continue;
        }
        let e = tag_acc.entry(t.tag_norm.as_str()).or_default();
        if e.label_date.is_none_or(|d| date >= d) {
            e.label = t.tag.trim_start_matches('#').to_string();
            e.label_date = Some(date);
        }
        e.minutes += t.minutes;
        e.sessions += 1;
        let ti = e.titles.entry(t.title_norm.as_str()).or_default();
        if ti.label_date.is_none_or(|d| date >= d) {
            ti.label = t.title.clone();
            ti.label_date = Some(date);
        }
        ti.minutes += t.minutes;
        ti.sessions += 1;
    }
    let mut tag_stats: Vec<TagStat> = tag_acc
        .into_iter()
        .filter(|(key, a)| {
            // The Untagged group only shows when it has time.
            if key.is_empty() {
                a.minutes > 0
            } else {
                a.minutes > 0 || a.sessions > 0
            }
        })
        .map(|(key, a)| {
            let mut titles: Vec<TagTitle> = a
                .titles
                .into_iter()
                .map(|(k, t)| TagTitle {
                    label: t.label,
                    key: k.to_string(),
                    minutes: t.minutes,
                    sessions: t.sessions,
                })
                .collect();
            titles.sort_by(|x, y| y.minutes.cmp(&x.minutes).then_with(|| x.key.cmp(&y.key)));
            TagStat {
                label: a.label,
                key: key.to_string(),
                minutes: a.minutes,
                sessions: a.sessions,
                titles,
            }
        })
        .collect();
    tag_stats.sort_by(|a, b| {
        a.key
            .is_empty()
            .cmp(&b.key.is_empty())
            .then(b.minutes.cmp(&a.minutes))
            .then_with(|| a.key.cmp(&b.key))
    });

    let overall_done: BTreeSet<Date> = overall
        .iter()
        .filter(|(_, (_, done))| *done)
        .map(|(d, _)| *d)
        .collect();
    let (current_streak, longest_streak) = streaks(&overall_done, today);
    let totals = Totals {
        minutes: total_minutes,
        sessions: total_sessions,
        active_days: overall_done.iter().filter(|d| in_range(**d)).count() as i64,
        current_streak,
        longest_streak,
    };

    let calendar = (0..CALENDAR_DAYS)
        .rev()
        .map(|back| {
            let date = today - back.days();
            let (minutes, done) = overall.get(&date).copied().unwrap_or_default();
            CalendarDay {
                date: date.to_string(),
                minutes,
                done,
            }
        })
        .collect();

    let habits = habits(&titles.iter().map(|(k, a)| (*k, a)).collect(), today);

    Dashboard {
        range,
        from: from.map(|d| d.to_string()),
        to: today.to_string(),
        totals,
        titles: title_stats,
        tags: tag_stats,
        calendar,
        habits,
        indexing: false,
    }
}

fn habits(titles: &HashMap<&str, &TitleAcc>, today: Date) -> Vec<Habit> {
    let window_start = today - (HABIT_WINDOW_DAYS - 1).days();
    let strip_start = today - (STRIP_DAYS - 1).days();
    let mut ranked: Vec<(i64, i64, &str, &TitleAcc, BTreeSet<Date>)> = titles
        .iter()
        .filter_map(|(key, acc)| {
            let done: BTreeSet<Date> = acc
                .days
                .iter()
                .filter(|(_, (_, s, n))| *s > 0 || *n)
                .map(|(d, _)| *d)
                .collect();
            let in_window = done.range(window_start..=today).count() as i64;
            let in_strip = done.range(strip_start..=today).count() as i64;
            (in_window > 0).then_some((in_strip, in_window, *key, *acc, done))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| a.2.cmp(b.2)));
    ranked
        .into_iter()
        .take(MAX_HABITS)
        .map(|(_, _, key, acc, done)| {
            let (current_streak, longest_streak) = streaks(&done, today);
            let days = (0..STRIP_DAYS)
                .rev()
                .map(|back| {
                    let date = today - back.days();
                    let (minutes, sessions, note) =
                        acc.days.get(&date).copied().unwrap_or_default();
                    DayCell {
                        date: date.to_string(),
                        minutes,
                        done: sessions > 0 || note,
                        note,
                    }
                })
                .collect();
            Habit {
                label: acc.label.clone(),
                key: key.to_string(),
                current_streak,
                longest_streak,
                days,
            }
        })
        .collect()
}

/// Reads the per-day facts from the index and aggregates them.
pub fn load(conn: &Connection, range: DashRange, today: Date) -> rusqlite::Result<Dashboard> {
    let mut facts = Vec::new();
    // A block has a note when its body (mirror lines already stripped) has real text.
    let mut notes = conn.prepare(
        "SELECT date, title_norm, title FROM blocks
         WHERE trim(body, ' ' || char(9) || char(10) || char(13)) <> ''
         ORDER BY date, start, ord",
    )?;
    for row in notes.query_map([], |r| {
        Ok(DayFact {
            date: r.get(0)?,
            title_norm: r.get(1)?,
            title: r.get(2)?,
            minutes: 0,
            sessions: 0,
            has_note: true,
        })
    })? {
        facts.push(row?);
    }
    let mut sessions =
        conn.prepare("SELECT date, title_norm, title, minutes FROM sessions ORDER BY date, start")?;
    for row in sessions.query_map([], |r| {
        Ok(DayFact {
            date: r.get(0)?,
            title_norm: r.get(1)?,
            title: r.get(2)?,
            minutes: r.get(3)?,
            sessions: 1,
            has_note: false,
        })
    })? {
        facts.push(row?);
    }
    let mut tags = conn.prepare(
        "SELECT s.date, COALESCE(t.tag, ''), COALESCE(t.tag_norm, ''), s.title, s.title_norm, s.minutes
         FROM sessions s LEFT JOIN session_tags t ON t.session_id = s.id
         ORDER BY s.date, s.start",
    )?;
    let tag_facts = tags
        .query_map([], |r| {
            Ok(TagFact {
                date: r.get(0)?,
                tag: r.get(1)?,
                tag_norm: r.get(2)?,
                title: r.get(3)?,
                title_norm: r.get(4)?,
                minutes: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(build(range, today, &facts, &tag_facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn d(s: &str) -> Date {
        s.parse().unwrap()
    }

    /// A finished-session fact.
    fn sess(day: &str, title: &str, minutes: i64) -> DayFact {
        DayFact {
            date: day.into(),
            title_norm: crate::search::norm(title),
            title: title.into(),
            minutes,
            sessions: 1,
            has_note: false,
        }
    }

    /// A non-empty-note fact.
    fn note(day: &str, title: &str) -> DayFact {
        DayFact {
            date: day.into(),
            title_norm: crate::search::norm(title),
            title: title.into(),
            minutes: 0,
            sessions: 0,
            has_note: true,
        }
    }

    fn tag(day: &str, t: &str, minutes: i64) -> TagFact {
        tag_t(day, t, "", minutes)
    }

    /// A tag fact of a session titled `title`; an empty `t` is the untagged fact.
    fn tag_t(day: &str, t: &str, title: &str, minutes: i64) -> TagFact {
        TagFact {
            date: day.into(),
            tag: t.into(),
            tag_norm: crate::search::norm_tag(t),
            title: title.into(),
            title_norm: crate::search::norm(title),
            minutes,
        }
    }

    fn titles_of(t: &TagStat) -> Vec<(&str, &str, i64, i64)> {
        t.titles
            .iter()
            .map(|x| (x.label.as_str(), x.key.as_str(), x.minutes, x.sessions))
            .collect()
    }

    // Wed 2026-10-21.
    const TODAY: &str = "2026-10-21";

    fn dash(range: DashRange, facts: &[DayFact], tags: &[TagFact]) -> Dashboard {
        build(range, d(TODAY), facts, tags)
    }

    // ---- ranges ----

    #[test]
    fn range_starts() {
        let wed = date(2026, 10, 21);
        assert_eq!(range_start(DashRange::Week, wed), Some(date(2026, 10, 19)));
        assert_eq!(range_start(DashRange::Month, wed), Some(date(2026, 10, 1)));
        assert_eq!(range_start(DashRange::Days30, wed), Some(date(2026, 9, 22)));
        assert_eq!(range_start(DashRange::Year, wed), Some(date(2026, 1, 1)));
        assert_eq!(range_start(DashRange::All, wed), None);
    }

    #[test]
    fn week_starts_on_monday_even_when_today_is_monday_or_sunday() {
        assert_eq!(
            range_start(DashRange::Week, date(2026, 10, 19)),
            Some(date(2026, 10, 19))
        );
        assert_eq!(
            range_start(DashRange::Week, date(2026, 10, 25)),
            Some(date(2026, 10, 19))
        );
    }

    #[test]
    fn week_and_month_cross_the_year_boundary() {
        // Thu 2026-01-01: the week began Mon 2025-12-29, the month and year today.
        let ny = date(2026, 1, 1);
        assert_eq!(range_start(DashRange::Week, ny), Some(date(2025, 12, 29)));
        assert_eq!(range_start(DashRange::Month, ny), Some(ny));
        assert_eq!(range_start(DashRange::Year, ny), Some(ny));
        assert_eq!(range_start(DashRange::Days30, ny), Some(date(2025, 12, 3)));
    }

    #[test]
    fn range_filters_totals_titles_and_tags() {
        let facts = [
            sess("2026-10-18", "A", 60), // Sunday, before this week
            sess("2026-10-19", "A", 30),
            sess("2026-10-21", "A", 15),
        ];
        let tags = [tag("2026-10-18", "x", 60), tag("2026-10-19", "x", 30)];
        let w = dash(DashRange::Week, &facts, &tags);
        assert_eq!(
            (w.totals.minutes, w.totals.sessions, w.totals.active_days),
            (45, 2, 2)
        );
        assert_eq!(w.titles[0].minutes, 45);
        assert_eq!(w.tags[0].minutes, 30);
        assert_eq!(w.from.as_deref(), Some("2026-10-19"));
        assert_eq!(w.to, TODAY);
        let all = dash(DashRange::All, &facts, &tags);
        assert_eq!((all.totals.minutes, all.totals.sessions), (105, 3));
        assert_eq!(all.from, None);
        assert_eq!(all.tags[0].minutes, 90);
    }

    #[test]
    fn year_range_excludes_last_year() {
        let facts = [sess("2025-12-31", "A", 60), sess("2026-01-01", "A", 20)];
        assert_eq!(dash(DashRange::Year, &facts, &[]).totals.minutes, 20);
        assert_eq!(dash(DashRange::All, &facts, &[]).totals.minutes, 80);
    }

    #[test]
    fn future_and_unparseable_dates_are_ignored() {
        let facts = [sess("2026-10-22", "A", 60), sess("garbage", "A", 60)];
        let dash = dash(DashRange::All, &facts, &[]);
        assert_eq!(dash.totals.minutes, 0);
        assert!(dash.titles.is_empty());
    }

    // ---- titles ----

    #[test]
    fn titles_group_by_norm_and_label_is_most_recent_original_case() {
        let facts = [
            sess("2026-10-19", "deep  work", 30),
            sess("2026-10-21", "Deep Work", 30),
            sess("2026-10-20", "DEEP WORK", 30),
        ];
        let d = dash(DashRange::All, &facts, &[]);
        assert_eq!(d.titles.len(), 1);
        let t = &d.titles[0];
        assert_eq!(
            (t.label.as_str(), t.key.as_str()),
            ("Deep Work", "deep work")
        );
        assert_eq!((t.minutes, t.sessions, t.done_days), (90, 3, 3));
    }

    #[test]
    fn untitled_is_excluded_from_titles_but_counts_in_totals() {
        let facts = [sess("2026-10-21", "", 25), sess("2026-10-21", "A", 5)];
        let d = dash(DashRange::All, &facts, &[]);
        assert_eq!(d.titles.len(), 1);
        assert_eq!(d.totals.minutes, 30);
        assert_eq!(d.totals.active_days, 1);
        assert!(d.habits.iter().all(|h| !h.key.is_empty()));
    }

    #[test]
    fn titles_sorted_by_minutes_then_done_days_then_key() {
        let facts = [
            sess("2026-10-21", "b", 10),
            sess("2026-10-20", "a", 10),
            sess("2026-10-21", "a", 0),
            note("2026-10-19", "c"),
            note("2026-10-20", "c"),
            sess("2026-10-21", "z", 50),
        ];
        let keys: Vec<_> = dash(DashRange::All, &facts, &[])
            .titles
            .into_iter()
            .map(|t| t.key)
            .collect();
        // z(50m), a(10m, 2 days), b(10m, 1 day), c(0m, 2 days)
        assert_eq!(keys, ["z", "a", "b", "c"]);
    }

    #[test]
    fn note_only_title_appears_with_zero_minutes() {
        let d = dash(DashRange::Week, &[note("2026-10-20", "Read")], &[]);
        let t = &d.titles[0];
        assert_eq!((t.minutes, t.sessions, t.done_days), (0, 0, 1));
    }

    #[test]
    fn done_days_dedupe_session_and_note_on_the_same_day() {
        let facts = [
            sess("2026-10-20", "A", 10),
            sess("2026-10-20", "A", 10),
            note("2026-10-20", "A"),
        ];
        let d = dash(DashRange::All, &facts, &[]);
        assert_eq!(d.titles[0].done_days, 1);
        assert_eq!(d.totals.active_days, 1);
    }

    #[test]
    fn title_with_activity_only_outside_the_range_is_dropped() {
        let d = dash(DashRange::Week, &[sess("2026-09-01", "Old", 60)], &[]);
        assert!(d.titles.is_empty());
    }

    // ---- tags ----

    #[test]
    fn tags_sum_minutes_and_sessions_with_recent_label() {
        let tags = [
            tag("2026-10-19", "#Mestrado", 30),
            tag("2026-10-20", "mestrado", 45),
            tag("2026-10-20", "headline", 10),
        ];
        let d = dash(DashRange::All, &[], &tags);
        assert_eq!(d.tags.len(), 2);
        assert_eq!(
            (
                d.tags[0].label.as_str(),
                d.tags[0].key.as_str(),
                d.tags[0].minutes,
                d.tags[0].sessions
            ),
            ("mestrado", "mestrado", 75, 2)
        );
        assert_eq!(d.tags[1].key, "headline");
    }

    #[test]
    fn tag_titles_nest_and_sum_to_the_tag() {
        let tags = [
            tag_t("2026-10-19", "m", "Thesis", 30),
            tag_t("2026-10-20", "m", "thesis", 20),
            tag_t("2026-10-21", "m", "Reading", 10),
        ];
        let d = dash(DashRange::All, &[], &tags);
        let m = &d.tags[0];
        assert_eq!((m.minutes, m.sessions), (60, 3));
        assert_eq!(
            titles_of(m),
            [("thesis", "thesis", 50, 2), ("Reading", "reading", 10, 1)]
        );
        assert_eq!(m.titles.iter().map(|t| t.minutes).sum::<i64>(), m.minutes);
        assert_eq!(m.titles.iter().map(|t| t.sessions).sum::<i64>(), m.sessions);
    }

    #[test]
    fn multi_tag_session_counts_under_both_tags() {
        let tags = [
            tag_t("2026-10-21", "a", "Deep", 40),
            tag_t("2026-10-21", "b", "Deep", 40),
        ];
        let d = dash(DashRange::All, &[], &tags);
        assert_eq!(d.tags.len(), 2);
        for t in &d.tags {
            assert_eq!((t.minutes, t.sessions), (40, 1));
            assert_eq!(titles_of(t), [("Deep", "deep", 40, 1)]);
        }
    }

    #[test]
    fn untagged_group_is_last_and_omitted_when_empty() {
        let tags = [
            tag_t("2026-10-21", "", "Loose", 500),
            tag_t("2026-10-21", "x", "A", 5),
        ];
        let d = dash(DashRange::All, &[], &tags);
        let keys: Vec<_> = d.tags.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["x", ""]);
        let u = &d.tags[1];
        assert_eq!((u.label.as_str(), u.minutes, u.sessions), ("", 500, 1));
        assert_eq!(titles_of(u), [("Loose", "loose", 500, 1)]);

        let zero = [
            tag_t("2026-10-21", "", "Loose", 0),
            tag_t("2026-10-21", "x", "A", 5),
        ];
        let d = dash(DashRange::All, &[], &zero);
        assert_eq!(d.tags.len(), 1);
        assert_eq!(d.tags[0].key, "x");
    }

    #[test]
    fn untitled_session_inside_a_tag_has_empty_label_and_key() {
        let tags = [
            tag_t("2026-10-21", "x", "", 25),
            tag_t("2026-10-21", "x", "A", 5),
        ];
        let d = dash(DashRange::All, &[], &tags);
        assert_eq!(titles_of(&d.tags[0]), [("", "", 25, 1), ("A", "a", 5, 1)]);
    }

    #[test]
    fn tag_titles_sorted_by_minutes_then_key() {
        let tags = [
            tag_t("2026-10-21", "x", "b", 10),
            tag_t("2026-10-21", "x", "a", 10),
            tag_t("2026-10-21", "x", "z", 30),
        ];
        let d = dash(DashRange::All, &[], &tags);
        let keys: Vec<_> = d.tags[0].titles.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["z", "a", "b"]);
    }

    #[test]
    fn tag_titles_respect_the_range_and_json_shape() {
        let tags = [
            tag_t("2026-10-01", "x", "Old", 99),
            tag_t("2026-10-21", "x", "New", 5),
        ];
        let d = dash(DashRange::Week, &[], &tags);
        assert_eq!(titles_of(&d.tags[0]), [("New", "new", 5, 1)]);
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["tags"][0]["titles"][0]["label"], "New");
        assert_eq!(v["tags"][0]["titles"][0]["key"], "new");
        assert_eq!(v["tags"][0]["titles"][0]["minutes"], 5);
        assert_eq!(v["tags"][0]["titles"][0]["sessions"], 1);
    }

    // ---- done = session OR note ----

    #[test]
    fn calendar_done_is_session_or_note() {
        let facts = [
            sess("2026-10-21", "A", 40),
            note("2026-10-20", "B"),
            sess("2026-10-19", "C", 0), // a session with no whole minute still counts as done
        ];
        let d = dash(DashRange::All, &facts, &[]);
        let cell = |s: &str| d.calendar.iter().find(|c| c.date == s).unwrap().clone();
        assert_eq!(
            (cell("2026-10-21").minutes, cell("2026-10-21").done),
            (40, true)
        );
        assert_eq!(
            (cell("2026-10-20").minutes, cell("2026-10-20").done),
            (0, true)
        );
        assert!(cell("2026-10-19").done);
        assert!(!cell("2026-10-18").done);
    }

    // ---- streaks ----

    fn set(days: &[&str]) -> BTreeSet<Date> {
        days.iter().map(|s| d(s)).collect()
    }

    #[test]
    fn streak_empty() {
        assert_eq!(streaks(&BTreeSet::new(), d(TODAY)), (0, 0));
    }

    #[test]
    fn streak_ending_today() {
        let s = set(&["2026-10-19", "2026-10-20", "2026-10-21"]);
        assert_eq!(streaks(&s, d(TODAY)), (3, 3));
    }

    #[test]
    fn streak_stays_alive_when_today_is_not_done_yet() {
        let s = set(&["2026-10-19", "2026-10-20"]);
        assert_eq!(streaks(&s, d(TODAY)), (2, 2));
    }

    #[test]
    fn streak_breaks_after_a_missed_day() {
        let s = set(&["2026-10-18", "2026-10-19"]); // yesterday and today both missed
        assert_eq!(streaks(&s, d(TODAY)), (0, 2));
    }

    #[test]
    fn longest_streak_can_be_older_than_the_current_one() {
        let s = set(&[
            "2026-09-01",
            "2026-09-02",
            "2026-09-03",
            "2026-09-04",
            "2026-10-20",
            "2026-10-21",
        ]);
        assert_eq!(streaks(&s, d(TODAY)), (2, 4));
    }

    #[test]
    fn streak_ignores_future_days() {
        let s = set(&["2026-10-21", "2026-10-22", "2026-10-23"]);
        assert_eq!(streaks(&s, d(TODAY)), (1, 1));
    }

    #[test]
    fn streak_crosses_month_and_year_boundaries() {
        let s = set(&["2025-12-30", "2025-12-31", "2026-01-01"]);
        assert_eq!(streaks(&s, d("2026-01-01")), (3, 3));
    }

    #[test]
    fn title_streaks_ignore_the_range() {
        let facts = [
            sess("2026-10-19", "A", 5),
            sess("2026-10-20", "A", 5),
            sess("2026-10-21", "A", 5),
            sess("2026-10-01", "A", 5),
        ];
        let d = dash(DashRange::Week, &facts, &[]);
        assert_eq!(
            (d.titles[0].current_streak, d.titles[0].longest_streak),
            (3, 3)
        );
        let month = dash(DashRange::Month, &facts[..3], &[]);
        assert_eq!(month.titles[0].current_streak, 3);
    }

    #[test]
    fn overall_streak_counts_any_title() {
        let facts = [
            sess("2026-10-20", "A", 5),
            note("2026-10-21", "B"),
            sess("2026-10-19", "C", 5),
        ];
        let d = dash(DashRange::All, &facts, &[]);
        assert_eq!((d.totals.current_streak, d.totals.longest_streak), (3, 3));
        assert_eq!((d.titles.iter().map(|t| t.current_streak).max()), Some(1));
    }

    // ---- calendar ----

    #[test]
    fn calendar_has_53_weeks_every_day_oldest_first() {
        let d = dash(DashRange::Week, &[sess("2026-10-21", "A", 5)], &[]);
        assert_eq!(d.calendar.len(), 371);
        assert_eq!(d.calendar.last().unwrap().date, TODAY);
        assert_eq!(d.calendar[0].date, "2025-10-16");
        assert!(d.calendar.windows(2).all(|w| w[0].date < w[1].date));
        assert_eq!(d.calendar.iter().filter(|c| c.minutes > 0).count(), 1);
        assert_eq!(d.calendar.iter().filter(|c| !c.done).count(), 370);
    }

    #[test]
    fn calendar_and_habits_ignore_the_range() {
        let facts = [sess("2026-09-01", "A", 5), sess("2026-10-21", "A", 5)];
        let week = dash(DashRange::Week, &facts, &[]);
        let all = dash(DashRange::All, &facts, &[]);
        assert_eq!(week.calendar, all.calendar);
        assert_eq!(week.habits, all.habits);
    }

    // ---- habits ----

    #[test]
    fn habit_strip_is_30_days_oldest_first_with_note_flag() {
        let facts = [
            sess("2026-10-21", "A", 20),
            note("2026-10-20", "A"),
            sess("2026-10-19", "A", 10),
            note("2026-10-19", "A"),
        ];
        let h = &dash(DashRange::All, &facts, &[]).habits[0];
        assert_eq!(h.days.len(), 30);
        assert_eq!(h.days[0].date, "2026-09-22");
        assert_eq!(h.days[29].date, TODAY);
        assert_eq!(
            (h.days[29].minutes, h.days[29].done, h.days[29].note),
            (20, true, false)
        );
        assert_eq!(
            (h.days[28].minutes, h.days[28].done, h.days[28].note),
            (0, true, true)
        );
        assert_eq!(
            (h.days[27].minutes, h.days[27].done, h.days[27].note),
            (10, true, true)
        );
        assert!(!h.days[26].done);
        assert_eq!((h.current_streak, h.longest_streak), (3, 3));
        assert_eq!(h.label, "A");
    }

    #[test]
    fn habits_need_a_done_day_in_the_last_90_days() {
        let facts = [
            sess("2026-07-24", "edge", 5),  // 89 days back: inside
            sess("2026-07-23", "stale", 5), // 90 days back: outside
        ];
        let keys: Vec<_> = dash(DashRange::All, &facts, &[])
            .habits
            .into_iter()
            .map(|h| h.key)
            .collect();
        assert_eq!(keys, ["edge"]);
    }

    #[test]
    fn habits_ordered_by_done_days_in_last_30_then_90() {
        let mut facts = vec![
            sess("2026-10-21", "once", 5),
            // 3 done days in the strip
            sess("2026-10-21", "often", 5),
            sess("2026-10-20", "often", 5),
            sess("2026-10-19", "often", 5),
            // 1 in the strip, plus 2 older ones inside 90 days
            sess("2026-10-21", "older", 5),
            sess("2026-08-01", "older", 5),
            sess("2026-08-02", "older", 5),
        ];
        facts.push(note("2026-10-21", "zzz"));
        let keys: Vec<_> = dash(DashRange::All, &facts, &[])
            .habits
            .into_iter()
            .map(|h| h.key)
            .collect();
        assert_eq!(keys, ["often", "older", "once", "zzz"]);
    }

    #[test]
    fn habits_are_capped_at_12() {
        let facts: Vec<_> = (0..20)
            .map(|i| sess("2026-10-21", &format!("t{i:02}"), 5))
            .collect();
        let habits = dash(DashRange::All, &facts, &[]).habits;
        assert_eq!(habits.len(), 12);
        assert_eq!(habits[0].key, "t00");
    }

    #[test]
    fn empty_input_gives_empty_but_complete_dashboard() {
        let d = dash(DashRange::Month, &[], &[]);
        assert_eq!(
            d.totals,
            Totals {
                minutes: 0,
                sessions: 0,
                active_days: 0,
                current_streak: 0,
                longest_streak: 0
            }
        );
        assert!(d.titles.is_empty() && d.tags.is_empty() && d.habits.is_empty());
        assert_eq!(d.calendar.len(), 371);
        assert!(!d.indexing);
    }

    #[test]
    fn json_shape_uses_camel_case_and_range_names() {
        let d = dash(
            DashRange::Days30,
            &[sess("2026-10-21", "A", 5)],
            &[tag("2026-10-21", "x", 5)],
        );
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["range"], "30d");
        assert_eq!(v["from"], "2026-09-22");
        assert_eq!(v["totals"]["activeDays"], 1);
        assert_eq!(v["titles"][0]["doneDays"], 1);
        assert_eq!(v["titles"][0]["currentStreak"], 1);
        assert_eq!(v["habits"][0]["longestStreak"], 1);
        assert_eq!(v["calendar"][370]["done"], true);
        let all = serde_json::to_value(dash(DashRange::All, &[], &[])).unwrap();
        assert!(all["from"].is_null());
        for (s, r) in [
            ("week", DashRange::Week),
            ("month", DashRange::Month),
            ("30d", DashRange::Days30),
            ("year", DashRange::Year),
            ("all", DashRange::All),
        ] {
            assert_eq!(
                serde_json::from_str::<DashRange>(&format!("\"{s}\"")).unwrap(),
                r
            );
        }
    }
}
