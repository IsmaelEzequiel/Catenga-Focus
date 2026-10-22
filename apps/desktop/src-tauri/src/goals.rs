//! Goals: rows of `catenga.sqlite` (a project and/or tags and/or a title, hours
//! per day, N days or indefinite), their validation, the one-time import of the
//! old `goals.json`, and the pure progress calculation over finished timer
//! minutes.

use crate::app_db::{AppDb, Project, ProjectRow, flag_in, read_projects, set_flag_in};
use crate::search::norm;
use crate::tags::{self, DEFAULT_COLOR, Tag};
use jiff::ToSpan;
use jiff::civil::Date;
use rusqlite::{Connection, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub const LEGACY_FILE: &str = "goals.json";
const IMPORTED: &str = "imported_goals_json";
const MIN_MINUTES: u32 = 5;
const MAX_MINUTES: u32 = 720;
const STRIP_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub id: i64,
    pub project_id: Option<i64>,
    pub tag_ids: Vec<i64>,
    pub title: Option<String>,
    pub minutes_per_day: u32,
    pub start: String,
    pub days: Option<u32>,
    pub archived: bool,
}

/// Tags by name: unknown names are created.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalDraft {
    pub project_id: Option<i64>,
    #[serde(default)]
    pub tag_names: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub minutes_per_day: u32,
    pub start: String,
    pub days: Option<u32>,
}

/// A goal with the rows it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalView {
    pub goal: Goal,
    pub project: Option<ProjectRow>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Upcoming,
    Active,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayCell {
    pub date: String,
    pub minutes: i64,
    pub met: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalProgress {
    pub goal: Goal,
    pub project: Option<Project>,
    pub tags: Vec<Tag>,
    pub status: Status,
    pub today_minutes: i64,
    pub elapsed_days: i64,
    pub met_days: i64,
    pub total_minutes: i64,
    pub current_streak: i64,
    pub longest_streak: i64,
    pub end: Option<String>,
    pub last14: Vec<DayCell>,
}

/// What a created goal reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalWrite {
    pub id: i64,
    /// Some tag of the draft did not exist yet.
    pub tags_created: bool,
}

// ---------------------------------------------------------------- validation

/// Strict `YYYY-MM-DD`.
fn parse_date(s: &str) -> Option<Date> {
    if s.len() != 10 {
        return None;
    }
    s.parse().ok()
}

fn check_numbers(minutes: u32, days: Option<u32>, start: &str) -> Result<(), String> {
    if !(MIN_MINUTES..=MAX_MINUTES).contains(&minutes) {
        return Err(format!(
            "Minutes per day must be {MIN_MINUTES}–{MAX_MINUTES}"
        ));
    }
    if days == Some(0) {
        return Err("Days must be at least 1".into());
    }
    if parse_date(start).is_none() {
        return Err("Start must be a date".into());
    }
    Ok(())
}

struct Clean {
    project_id: Option<i64>,
    tag_names: Vec<String>,
    title: Option<String>,
}

fn validate(draft: &GoalDraft) -> Result<Clean, String> {
    let mut tag_names: Vec<String> = Vec::new();
    for name in &draft.tag_names {
        let name = tags::clean_name(name)?;
        if !tag_names.iter().any(|n| norm(n) == norm(&name)) {
            tag_names.push(name);
        }
    }
    let title = draft
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    if draft.project_id.is_none() && tag_names.is_empty() && title.is_none() {
        return Err("Pick a project, a tag or a title".into());
    }
    check_numbers(draft.minutes_per_day, draft.days, &draft.start)?;
    Ok(Clean {
        project_id: draft.project_id,
        tag_names,
        title,
    })
}

// ------------------------------------------------------------------ database

/// Points the goal at exactly `names`, creating unknown tags; true if any was created.
fn set_goal_tags(
    tx: &Connection,
    goal_id: i64,
    names: &[String],
    now: i64,
) -> rusqlite::Result<bool> {
    tx.execute("DELETE FROM goal_tags WHERE goal_id = ?1", [goal_id])?;
    let mut created_any = false;
    for name in names {
        let (tag_id, created) = tags::ensure_tag(tx, name, DEFAULT_COLOR, now)?;
        created_any |= created;
        tx.execute(
            "INSERT OR IGNORE INTO goal_tags(goal_id, tag_id) VALUES (?1, ?2)",
            params![goal_id, tag_id],
        )?;
    }
    Ok(created_any)
}

fn check_project(tx: &Connection, id: Option<i64>) -> rusqlite::Result<Result<(), &'static str>> {
    let Some(id) = id else { return Ok(Ok(())) };
    let n: i64 = tx.query_row("SELECT count(*) FROM projects WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    Ok(if n == 0 {
        Err("Project not found")
    } else {
        Ok(())
    })
}

fn read_views(conn: &Connection, archived: bool) -> rusqlite::Result<Vec<GoalView>> {
    let projects = read_projects(conn)?;
    let mut all_tags = conn.prepare(
        "SELECT t.id, t.name, t.color,
                (SELECT count(*) FROM goal_tags x WHERE x.tag_id = t.id)
         FROM goal_tags g JOIN tags t ON t.id = g.tag_id
         WHERE g.goal_id = ?1 ORDER BY t.name_norm",
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, title, minutes_per_day, start, days, archived
         FROM goals WHERE archived = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([archived], |r| {
            Ok(Goal {
                id: r.get(0)?,
                project_id: r.get(1)?,
                tag_ids: Vec::new(),
                title: r.get(2)?,
                minutes_per_day: r.get(3)?,
                start: r.get(4)?,
                days: r.get(5)?,
                archived: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for mut goal in rows {
        let tags = all_tags
            .query_map([goal.id], |r| {
                Ok(Tag {
                    id: Some(r.get(0)?),
                    name: r.get(1)?,
                    color: r.get(2)?,
                    saved: true,
                    goal_count: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        goal.tag_ids = tags.iter().filter_map(|t| t.id).collect();
        let project = goal
            .project_id
            .and_then(|id| projects.iter().find(|p| p.project.id == id).cloned());
        out.push(GoalView {
            goal,
            project,
            tags,
        });
    }
    Ok(out)
}

impl AppDb {
    pub fn goal_create(&self, draft: &GoalDraft, now: i64) -> Result<GoalWrite, String> {
        let clean = validate(draft)?;
        self.with_conn("saving goal", |conn| {
            let tx = conn.transaction()?;
            if let Err(m) = check_project(&tx, clean.project_id)? {
                return Ok(Err(m.to_string()));
            }
            tx.execute(
                "INSERT INTO goals(project_id, title, minutes_per_day, start, days, archived,
                                   created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
                params![
                    clean.project_id,
                    clean.title,
                    draft.minutes_per_day,
                    draft.start,
                    draft.days,
                    now
                ],
            )?;
            let id = tx.last_insert_rowid();
            let tags_created = set_goal_tags(&tx, id, &clean.tag_names, now)?;
            tx.commit()?;
            Ok(Ok(GoalWrite { id, tags_created }))
        })?
    }

    /// Replaces everything but the id and the archived flag; returns whether tags were created.
    pub fn goal_edit(&self, id: i64, draft: &GoalDraft, now: i64) -> Result<bool, String> {
        let clean = validate(draft)?;
        self.with_conn("saving goal", |conn| {
            let tx = conn.transaction()?;
            if let Err(m) = check_project(&tx, clean.project_id)? {
                return Ok(Err(m.to_string()));
            }
            let changed = tx.execute(
                "UPDATE goals SET project_id = ?2, title = ?3, minutes_per_day = ?4, start = ?5,
                                  days = ?6, updated_at = ?7 WHERE id = ?1",
                params![
                    id,
                    clean.project_id,
                    clean.title,
                    draft.minutes_per_day,
                    draft.start,
                    draft.days,
                    now
                ],
            )?;
            if changed == 0 {
                return Ok(Err("Goal not found".to_string()));
            }
            let created = set_goal_tags(&tx, id, &clean.tag_names, now)?;
            tx.commit()?;
            Ok(Ok(created))
        })?
    }

    pub fn goal_set_archived(&self, id: i64, archived: bool, now: i64) -> Result<(), String> {
        let changed = self.with_conn("updating goal", |conn| {
            conn.execute(
                "UPDATE goals SET archived = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, archived, now],
            )
        })?;
        if changed == 0 {
            return Err("Goal not found".into());
        }
        Ok(())
    }

    /// Removes the goal; its tags stay saved.
    pub fn goal_delete(&self, id: i64) -> Result<(), String> {
        let changed = self.with_conn("deleting goal", |conn| {
            conn.execute("DELETE FROM goals WHERE id = ?1", [id])
        })?;
        if changed == 0 {
            return Err("Goal not found".into());
        }
        Ok(())
    }

    /// The archived (or the live) goals with their project and tags, by id.
    pub fn goal_views(&self, archived: bool) -> Result<Vec<GoalView>, String> {
        self.with_conn("reading goals", |conn| read_views(conn, archived))
    }

    /// One-time import of the M8 `goals.json`; afterwards the file is renamed to
    /// `goals.json.imported` (never deleted). An invalid file is an error and stays.
    pub fn import_goals_json(&self, root: &Path, now: i64) -> Result<(), String> {
        if self.flag(IMPORTED)? {
            return Ok(());
        }
        let path = root.join(LEGACY_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let bad = |why: &dyn std::fmt::Display| {
            format!(
                "{} could not be imported ({why}); fix or move it — your goals will appear once it is valid",
                path.display()
            )
        };
        let legacy = parse_legacy(&bytes).map_err(|e| bad(&e))?;
        let imported = self.with_conn("importing goals", |conn| {
            let tx = conn.transaction()?;
            if flag_in(&tx, IMPORTED)? {
                return Ok(false);
            }
            for g in &legacy {
                tx.execute(
                    "INSERT INTO goals(project_id, title, minutes_per_day, start, days, archived,
                                       created_at, updated_at)
                     VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![g.title, g.minutes_per_day, g.start, g.days, g.archived, now],
                )?;
                if let Some(tag) = &g.tag {
                    let id = tx.last_insert_rowid();
                    set_goal_tags(&tx, id, std::slice::from_ref(tag), now)?;
                }
            }
            set_flag_in(&tx, IMPORTED)?;
            tx.commit()?;
            Ok(true)
        })?;
        if imported && let Err(e) = rename_imported(&path) {
            // The flag is set, so nothing is imported twice; the file just stays.
            eprintln!("renaming {}: {e}", path.display());
        }
        Ok(())
    }
}

/// `goals.json.imported`, or `-2`, `-3`… when taken.
fn rename_imported(path: &Path) -> std::io::Result<()> {
    let base = format!("{}.imported", path.display());
    let mut target = std::path::PathBuf::from(&base);
    let mut n = 2;
    while target.exists() {
        target = std::path::PathBuf::from(format!("{base}-{n}"));
        n += 1;
    }
    fs::rename(path, target)
}

struct LegacyGoal {
    title: Option<String>,
    tag: Option<String>,
    minutes_per_day: u32,
    start: String,
    days: Option<u32>,
    archived: bool,
}

#[derive(Deserialize)]
struct LegacyFile {
    version: u32,
    #[serde(default)]
    goals: Vec<LegacyEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyEntry {
    id: String,
    subject: LegacySubject,
    minutes_per_day: u32,
    start: String,
    days: Option<u32>,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
struct LegacySubject {
    kind: String,
    value: String,
}

fn parse_legacy(bytes: &[u8]) -> Result<Vec<LegacyGoal>, String> {
    let file: LegacyFile = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != 1 {
        return Err(format!("unsupported version {}", file.version));
    }
    file.goals
        .into_iter()
        .map(|g| {
            check_numbers(g.minutes_per_day, g.days, &g.start)
                .map_err(|e| format!("goal {}: {e}", g.id))?;
            let (title, tag) = match g.subject.kind.as_str() {
                "title" => {
                    let t = g.subject.value.trim();
                    if t.is_empty() {
                        return Err(format!("goal {}: empty title", g.id));
                    }
                    (Some(t.to_string()), None)
                }
                "tag" => (
                    None,
                    Some(
                        tags::clean_name(&g.subject.value)
                            .map_err(|e| format!("goal {}: {e}", g.id))?,
                    ),
                ),
                other => return Err(format!("goal {}: unknown subject kind {other}", g.id)),
            };
            Ok(LegacyGoal {
                title,
                tag,
                minutes_per_day: g.minutes_per_day,
                start: g.start,
                days: g.days,
                archived: g.archived,
            })
        })
        .collect()
}

// ------------------------------------------------------------------ progress

fn met(minutes: &BTreeMap<Date, i64>, day: Date, target: i64) -> bool {
    minutes.get(&day).copied().unwrap_or(0) >= target
}

/// Progress of `goal` as of `today`; `minutes` are finished-session minutes per
/// local day (the session's start day) matching the goal's subject.
pub fn progress(goal: &Goal, minutes: &BTreeMap<Date, i64>, today: Date) -> GoalProgress {
    let target = i64::from(goal.minutes_per_day);
    // Goals read from the file are validated, so the start parses.
    let start = parse_date(&goal.start).unwrap_or(today);
    let end = goal.days.map(|d| start + (i64::from(d) - 1).days());
    let status = if start > today {
        Status::Upcoming
    } else if end.is_some_and(|e| e < today) {
        Status::Finished
    } else {
        Status::Active
    };
    let mut p = GoalProgress {
        goal: goal.clone(),
        project: None,
        tags: Vec::new(),
        status,
        today_minutes: 0,
        elapsed_days: 0,
        met_days: 0,
        total_minutes: 0,
        current_streak: 0,
        longest_streak: 0,
        end: end.map(|e| e.to_string()),
        last14: Vec::new(),
    };
    if status == Status::Upcoming {
        return p;
    }
    let last = end.map_or(today, |e| e.min(today));
    p.elapsed_days = i64::from((last - start).get_days()) + 1;
    let day_at = |i: i64| start + i.days();
    let mut run = 0;
    for i in 0..p.elapsed_days {
        let day = day_at(i);
        let m = minutes.get(&day).copied().unwrap_or(0);
        p.total_minutes += m;
        if m >= target {
            p.met_days += 1;
            run += 1;
            p.longest_streak = p.longest_streak.max(run);
        } else {
            run = 0;
        }
    }
    // Same rule as M7 streaks: a day still in progress does not break the run.
    let mut cursor = last;
    if !met(minutes, last, target) && last == today {
        cursor = last - 1.day();
    }
    while cursor >= start && met(minutes, cursor, target) {
        p.current_streak += 1;
        cursor -= 1.day();
    }
    if (start..=last).contains(&today) {
        p.today_minutes = minutes.get(&today).copied().unwrap_or(0);
    }
    let first = (p.elapsed_days - STRIP_DAYS).max(0);
    p.last14 = (first..p.elapsed_days)
        .map(|i| {
            let day = day_at(i);
            let m = minutes.get(&day).copied().unwrap_or(0);
            DayCell {
                date: day.to_string(),
                minutes: m,
                met: m >= target,
            }
        })
        .collect();
    p
}

// ------------------------------------------------------------------- SQL

/// What a session must match (every present filter holds).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Google calendar id of the goal's project.
    pub calendar_id: Option<String>,
    /// Normalised tags; the session needs any of them.
    pub tag_norms: Vec<String>,
    pub title_norm: Option<String>,
}

impl GoalView {
    pub fn filter(&self) -> Filter {
        Filter {
            calendar_id: self.project.as_ref().map(|p| p.calendar_id.clone()),
            tag_norms: self
                .tags
                .iter()
                .map(|t| crate::search::norm_tag(&t.name))
                .collect(),
            title_norm: self.goal.title.as_deref().map(norm),
        }
    }
}

/// Finished minutes per session start day for the filter (same title/tag
/// normalisation as history and the dashboard).
pub fn minutes_by_day(conn: &Connection, filter: &Filter) -> rusqlite::Result<BTreeMap<Date, i64>> {
    let mut sql = String::from("SELECT date, SUM(minutes) FROM sessions s WHERE 1 = 1");
    let mut args: Vec<String> = Vec::new();
    if let Some(c) = &filter.calendar_id {
        args.push(c.clone());
        sql += &format!(" AND s.project = ?{}", args.len());
    }
    if !filter.tag_norms.is_empty() {
        let first = args.len() + 1;
        args.extend(filter.tag_norms.iter().cloned());
        let marks: Vec<String> = (first..=args.len()).map(|i| format!("?{i}")).collect();
        // EXISTS so a session carrying several matching tags is counted once.
        sql += &format!(
            " AND EXISTS (SELECT 1 FROM session_tags t WHERE t.session_id = s.id AND t.tag_norm IN ({}))",
            marks.join(", ")
        );
    }
    if let Some(t) = &filter.title_norm {
        args.push(t.clone());
        sql += &format!(" AND s.title_norm = ?{}", args.len());
    }
    sql += " GROUP BY date";
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (date, minutes) = row?;
        if let Some(day) = parse_date(&date) {
            out.insert(day, minutes);
        }
    }
    Ok(out)
}

/// Progress of the given goals in display order: active by start, then
/// upcoming by start, then finished (latest end first).
pub fn load(
    conn: &Connection,
    goals: &[GoalView],
    today: Date,
) -> rusqlite::Result<Vec<GoalProgress>> {
    let mut out = Vec::new();
    for view in goals {
        let minutes = minutes_by_day(conn, &view.filter())?;
        let mut p = progress(&view.goal, &minutes, today);
        p.project = view.project.as_ref().map(|r| r.project.clone());
        p.tags = view.tags.clone();
        out.push(p);
    }
    let rank = |s: Status| match s {
        Status::Active => 0,
        Status::Upcoming => 1,
        Status::Finished => 2,
    };
    out.sort_by(|a, b| {
        rank(a.status).cmp(&rank(b.status)).then_with(|| {
            if a.status == Status::Finished {
                b.end
                    .cmp(&a.end)
                    .then_with(|| a.goal.start.cmp(&b.goal.start))
            } else {
                a.goal.start.cmp(&b.goal.start)
            }
            .then_with(|| a.goal.id.cmp(&b.goal.id))
        })
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_db::test_util::{Fixture, cal, fixture};
    use crate::index::test_util::Env;
    use crate::sessions::{self, Mode, Session, SessionBlock};

    fn d_(s: &str) -> Date {
        s.parse().unwrap()
    }

    fn d(s: &str) -> Date {
        d_(s)
    }

    fn goal(start: &str, days: Option<u32>, per_day: u32) -> Goal {
        Goal {
            id: 1,
            project_id: None,
            tag_ids: vec![],
            title: Some("Rust".into()),
            minutes_per_day: per_day,
            start: start.into(),
            days,
            archived: false,
        }
    }

    fn mins(pairs: &[(&str, i64)]) -> BTreeMap<Date, i64> {
        pairs.iter().map(|(k, v)| (d(k), *v)).collect()
    }

    // ---- progress

    #[test]
    fn counts_met_elapsed_total_and_today() {
        let g = goal("2026-10-19", Some(30), 60);
        let m = mins(&[("2026-10-19", 60), ("2026-10-20", 30), ("2026-10-21", 70)]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.status, Status::Active);
        assert_eq!(p.elapsed_days, 3);
        assert_eq!(p.met_days, 2);
        assert_eq!(p.total_minutes, 160);
        assert_eq!(p.today_minutes, 70);
        assert_eq!(p.current_streak, 1);
        assert_eq!(p.longest_streak, 1);
        assert_eq!(p.end.as_deref(), Some("2026-11-17"));
    }

    #[test]
    fn today_not_met_yet_keeps_the_streak_alive() {
        let g = goal("2026-10-19", None, 60);
        let m = mins(&[("2026-10-19", 60), ("2026-10-20", 60), ("2026-10-21", 10)]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.current_streak, 2);
        let p = progress(
            &g,
            &mins(&[("2026-10-19", 60), ("2026-10-20", 60)]),
            d("2026-10-21"),
        );
        assert_eq!(p.current_streak, 2);
        // today met extends it
        let m = mins(&[("2026-10-19", 60), ("2026-10-20", 60), ("2026-10-21", 60)]);
        assert_eq!(progress(&g, &m, d("2026-10-21")).current_streak, 3);
    }

    #[test]
    fn missing_yesterday_breaks_the_streak() {
        let g = goal("2026-10-15", None, 60);
        let m = mins(&[("2026-10-15", 60), ("2026-10-16", 60), ("2026-10-17", 60)]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.current_streak, 0);
        assert_eq!(p.longest_streak, 3);
    }

    #[test]
    fn streaks_stay_inside_the_span() {
        let g = goal("2026-10-20", None, 60);
        let m = mins(&[
            ("2026-10-18", 99),
            ("2026-10-19", 99),
            ("2026-10-20", 60),
            ("2026-10-21", 60),
        ]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.current_streak, 2);
        assert_eq!(p.longest_streak, 2);
        assert_eq!(p.total_minutes, 120);
        assert_eq!(p.elapsed_days, 2);
    }

    #[test]
    fn longest_streak_picks_the_best_run() {
        let g = goal("2026-10-01", None, 10);
        let m = mins(&[
            ("2026-10-01", 10),
            ("2026-10-02", 10),
            ("2026-10-04", 10),
            ("2026-10-05", 10),
            ("2026-10-06", 10),
            ("2026-10-08", 10),
        ]);
        let p = progress(&g, &m, d("2026-10-08"));
        assert_eq!(p.longest_streak, 3);
        assert_eq!(p.current_streak, 1);
        assert_eq!(p.met_days, 6);
    }

    #[test]
    fn upcoming_goal_is_all_zero() {
        let g = goal("2026-10-22", Some(3), 60);
        let p = progress(
            &g,
            &mins(&[("2026-10-21", 90), ("2026-10-22", 90)]),
            d("2026-10-21"),
        );
        assert_eq!(p.status, Status::Upcoming);
        assert_eq!(
            (p.elapsed_days, p.met_days, p.total_minutes, p.today_minutes),
            (0, 0, 0, 0)
        );
        assert_eq!((p.current_streak, p.longest_streak), (0, 0));
        assert!(p.last14.is_empty());
        assert_eq!(p.end.as_deref(), Some("2026-10-24"));
    }

    #[test]
    fn start_today_is_active_with_one_elapsed_day() {
        let g = goal("2026-10-21", Some(1), 60);
        let p = progress(&g, &mins(&[("2026-10-21", 5)]), d("2026-10-21"));
        assert_eq!(p.status, Status::Active);
        assert_eq!(p.elapsed_days, 1);
        assert_eq!(p.today_minutes, 5);
        assert_eq!(p.last14.len(), 1);
    }

    #[test]
    fn finished_goal_caps_at_its_span() {
        let g = goal("2026-10-01", Some(3), 60);
        let m = mins(&[
            ("2026-09-30", 99),
            ("2026-10-01", 60),
            ("2026-10-02", 60),
            ("2026-10-03", 60),
            ("2026-10-04", 99),
            ("2026-10-21", 99),
        ]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.status, Status::Finished);
        assert_eq!(p.elapsed_days, 3);
        assert_eq!(p.met_days, 3);
        assert_eq!(p.total_minutes, 180);
        assert_eq!(p.today_minutes, 0);
        assert_eq!(p.last14.len(), 3);
        assert_eq!(p.last14.last().unwrap().date, "2026-10-03");
        // the run ended on the last day, so it is the final streak
        assert_eq!(p.current_streak, 3);
        assert_eq!(p.longest_streak, 3);
    }

    #[test]
    fn finished_goal_whose_last_day_was_missed_has_no_current_streak() {
        let g = goal("2026-10-01", Some(3), 60);
        let m = mins(&[("2026-10-01", 60), ("2026-10-02", 60)]);
        let p = progress(&g, &m, d("2026-10-21"));
        assert_eq!(p.current_streak, 0);
        assert_eq!(p.longest_streak, 2);
    }

    #[test]
    fn last_day_today_is_still_active() {
        let g = goal("2026-10-19", Some(3), 60);
        let p = progress(
            &g,
            &mins(&[("2026-10-19", 60), ("2026-10-20", 60)]),
            d("2026-10-21"),
        );
        assert_eq!(p.status, Status::Active);
        assert_eq!(p.elapsed_days, 3);
        assert_eq!(p.current_streak, 2);
    }

    #[test]
    fn indefinite_goal_never_finishes() {
        let g = goal("2026-01-01", None, 60);
        let p = progress(&g, &mins(&[]), d("2030-01-01"));
        assert_eq!(p.status, Status::Active);
        assert_eq!(p.end, None);
        assert_eq!(p.last14.len(), 14);
        assert_eq!(p.last14[13].date, "2030-01-01");
    }

    #[test]
    fn last14_window_edges() {
        let today = d("2026-10-21");
        // 13 days old: 13 cells
        let p = progress(&goal("2026-10-09", None, 60), &mins(&[]), today);
        assert_eq!(p.elapsed_days, 13);
        assert_eq!(p.last14.len(), 13);
        assert_eq!(p.last14[0].date, "2026-10-09");
        // exactly 14 days old
        let p = progress(&goal("2026-10-08", None, 60), &mins(&[]), today);
        assert_eq!(p.last14.len(), 14);
        assert_eq!(p.last14[0].date, "2026-10-08");
        // 15 days old: oldest dropped
        let m = mins(&[("2026-10-09", 30), ("2026-10-21", 60)]);
        let p = progress(&goal("2026-10-07", None, 60), &m, today);
        assert_eq!(p.last14.len(), 14);
        assert_eq!(p.last14[0].date, "2026-10-08");
        assert_eq!(p.last14[13].date, "2026-10-21");
        assert_eq!(
            p.last14[1],
            DayCell {
                date: "2026-10-09".into(),
                minutes: 30,
                met: false
            }
        );
        assert!(p.last14[13].met);
    }

    // ---- database

    fn draft() -> GoalDraft {
        GoalDraft {
            project_id: None,
            tag_names: vec!["rust".into()],
            title: None,
            minutes_per_day: 60,
            start: "2026-10-21".into(),
            days: Some(30),
        }
    }

    fn with_project(f: &Fixture) -> i64 {
        f.db.upsert_calendars(&[cal("mest@x", "Mestrado", false, true)], 1)
            .unwrap();
        f.db.projects().unwrap()[0].id
    }

    #[test]
    fn create_reads_back_with_project_tags_and_title() {
        let f = fixture();
        let pid = with_project(&f);
        let mut dr = draft();
        dr.project_id = Some(pid);
        dr.tag_names = vec!["#Rust".into(), "go".into(), "rust".into()];
        dr.title = Some("  Deep work ".into());
        let w = f.db.goal_create(&dr, 5).unwrap();
        assert_eq!(
            w,
            GoalWrite {
                id: 1,
                tags_created: true
            }
        );
        let views = f.db.goal_views(false).unwrap();
        assert_eq!(views.len(), 1);
        let v = &views[0];
        assert_eq!(v.goal.project_id, Some(pid));
        assert_eq!(v.goal.title.as_deref(), Some("Deep work"));
        let names: Vec<_> = v.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["go", "Rust"], "duplicates by norm collapse, sorted");
        assert_eq!(v.goal.tag_ids.len(), 2);
        assert_eq!(v.project.as_ref().unwrap().calendar_id, "mest@x");
        assert!(!v.goal.archived);
        // Reusing existing tags creates nothing.
        let w2 = f.db.goal_create(&draft(), 6).unwrap();
        assert!(!w2.tags_created);
        assert_eq!(f.db.saved_tags().unwrap().len(), 2);
    }

    #[test]
    fn validation_messages() {
        let f = fixture();
        let err = |d: GoalDraft| f.db.goal_create(&d, 1).unwrap_err();
        let mut d = draft();
        d.tag_names = vec![];
        assert_eq!(err(d.clone()), "Pick a project, a tag or a title");
        d.title = Some("   ".into());
        assert_eq!(err(d), "Pick a project, a tag or a title");
        let mut d = draft();
        d.minutes_per_day = 4;
        assert_eq!(err(d), "Minutes per day must be 5–720");
        let mut d = draft();
        d.minutes_per_day = 721;
        assert_eq!(err(d), "Minutes per day must be 5–720");
        let mut d = draft();
        d.days = Some(0);
        assert_eq!(err(d), "Days must be at least 1");
        let mut d = draft();
        d.start = "2026-1-1".into();
        assert_eq!(err(d), "Start must be a date");
        let mut d = draft();
        d.tag_names = vec!["two words".into()];
        assert_eq!(err(d), "Tag names can't contain spaces");
        let mut d = draft();
        d.project_id = Some(77);
        assert_eq!(err(d), "Project not found");
        assert!(f.db.goal_views(false).unwrap().is_empty());
        assert!(
            f.db.saved_tags().unwrap().is_empty(),
            "a refused goal creates no tag"
        );
    }

    #[test]
    fn edit_replaces_filters_keeps_id_and_archived_flag() {
        let f = fixture();
        let pid = with_project(&f);
        let id = f.db.goal_create(&draft(), 1).unwrap().id;
        f.db.goal_set_archived(id, true, 2).unwrap();
        let mut d = draft();
        d.project_id = Some(pid);
        d.tag_names = vec!["go".into()];
        d.minutes_per_day = 90;
        d.days = None;
        assert!(f.db.goal_edit(id, &d, 3).unwrap());
        let v = &f.db.goal_views(true).unwrap()[0];
        assert_eq!(v.goal.id, id);
        assert!(v.goal.archived);
        assert_eq!((v.goal.minutes_per_day, v.goal.days), (90, None));
        assert_eq!(v.tags.len(), 1);
        assert_eq!(v.tags[0].name, "go");
        assert_eq!(f.db.goal_edit(99, &d, 3), Err("Goal not found".into()));
        let mut bad = d.clone();
        bad.minutes_per_day = 1;
        assert!(f.db.goal_edit(id, &bad, 4).is_err());
        assert_eq!(f.db.goal_views(true).unwrap()[0].goal.minutes_per_day, 90);
    }

    #[test]
    fn archive_restore_and_delete_keep_tags() {
        let f = fixture();
        let id = f.db.goal_create(&draft(), 1).unwrap().id;
        f.db.goal_set_archived(id, true, 2).unwrap();
        assert!(f.db.goal_views(false).unwrap().is_empty());
        assert_eq!(f.db.goal_views(true).unwrap().len(), 1);
        f.db.goal_set_archived(id, false, 3).unwrap();
        assert_eq!(f.db.goal_views(false).unwrap().len(), 1);
        let tag_id = f.db.saved_tags().unwrap()[0].id.unwrap();
        assert_eq!(f.db.tag_delete(tag_id), Err("A goal uses this tag".into()));
        f.db.goal_delete(id).unwrap();
        assert!(f.db.goal_views(false).unwrap().is_empty());
        let tags = f.db.saved_tags().unwrap();
        assert_eq!(
            (tags.len(), tags[0].goal_count),
            (1, 0),
            "the tag outlives its goal"
        );
        assert_eq!(f.db.goal_delete(id), Err("Goal not found".into()));
        assert_eq!(
            f.db.goal_set_archived(id, true, 1),
            Err("Goal not found".into())
        );
        // ids are not reused
        assert_eq!(f.db.goal_create(&draft(), 4).unwrap().id, id + 1);
    }

    #[test]
    fn an_archived_project_stays_attached_to_its_goal() {
        let f = fixture();
        let pid = with_project(&f);
        let mut d = draft();
        d.project_id = Some(pid);
        f.db.goal_create(&d, 1).unwrap();
        // The calendar disappears from Google: archived, but the goal keeps pointing at it.
        f.db.upsert_calendars(&[], 2).unwrap();
        let v = &f.db.goal_views(false).unwrap()[0];
        assert!(v.project.as_ref().unwrap().project.archived);
        assert_eq!(v.goal.project_id, Some(pid));
    }

    #[test]
    fn deleting_a_project_row_unlinks_the_goal() {
        let f = fixture();
        let pid = with_project(&f);
        let mut d = draft();
        d.project_id = Some(pid);
        f.db.goal_create(&d, 1).unwrap();
        f.db.with_conn("t", |c| c.execute("DELETE FROM projects", []))
            .unwrap();
        assert_eq!(f.db.goal_views(false).unwrap()[0].goal.project_id, None);
    }

    #[test]
    fn goal_progress_json_shape() {
        let f = fixture();
        let pid = with_project(&f);
        let mut d = draft();
        d.project_id = Some(pid);
        f.db.goal_create(&d, 1).unwrap();
        let v = f.db.goal_views(false).unwrap().remove(0);
        let mut p = progress(&v.goal, &mins(&[("2026-10-21", 36)]), d_("2026-10-21"));
        p.project = v.project.map(|r| r.project);
        p.tags = v.tags;
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "goal": {"id": 1, "projectId": 1, "tagIds": [1], "title": null,
                         "minutesPerDay": 60, "start": "2026-10-21", "days": 30, "archived": false},
                "project": {"id": 1, "googleCalendarId": "mest@x", "name": "Mestrado",
                            "color": "#16a765", "showOnTimeline": true, "isPrimary": false, "archived": false},
                "tags": [{"id": 1, "name": "rust", "color": "sun", "saved": true, "goalCount": 1}],
                "status": "active", "todayMinutes": 36, "elapsedDays": 1, "metDays": 0,
                "totalMinutes": 36, "currentStreak": 0, "longestStreak": 0, "end": "2026-11-19",
                "last14": [{"date": "2026-10-21", "minutes": 36, "met": false}]
            })
        );
    }

    #[test]
    fn goal_draft_deserializes_camel_case() {
        let d: GoalDraft = serde_json::from_str(
            r#"{"projectId":null,"tagNames":["a"],"title":null,"minutesPerDay":30,"start":"2026-10-21","days":null}"#,
        )
        .unwrap();
        assert_eq!(
            (d.tag_names, d.minutes_per_day, d.days),
            (vec!["a".to_string()], 30, None)
        );
    }

    // ---- goals.json import

    const LEGACY: &str = r##"{
      "version": 1,
      "goals": [
        {"id":"2026-10-21-rust","subject":{"kind":"title","value":"Rust"},"minutesPerDay":60,"start":"2026-10-21","days":30,"archived":false},
        {"id":"2026-10-22-mest","subject":{"kind":"tag","value":"#mestrado"},"minutesPerDay":30,"start":"2026-10-22","days":null,"archived":true}
      ]}"##;

    fn notes(f: &Fixture) -> std::path::PathBuf {
        let root = f.tmp.0.join("notes");
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn legacy_goals_are_imported_once_and_the_file_renamed() {
        let f = fixture();
        f.db.tag_save("mestrado", "leaf", 1).unwrap();
        let root = notes(&f);
        fs::write(root.join("goals.json"), LEGACY).unwrap();
        f.db.import_goals_json(&root, 9).unwrap();
        assert!(!root.join("goals.json").exists());
        assert_eq!(
            fs::read_to_string(root.join("goals.json.imported")).unwrap(),
            LEGACY
        );
        let live = f.db.goal_views(false).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].goal.title.as_deref(), Some("Rust"));
        assert_eq!(live[0].goal.project_id, None);
        assert!(live[0].tags.is_empty());
        let arch = f.db.goal_views(true).unwrap();
        assert_eq!(arch.len(), 1);
        assert_eq!(arch[0].tags[0].name, "mestrado");
        assert_eq!(arch[0].tags[0].color, "leaf", "an existing tag is reused");
        assert_eq!(
            (arch[0].goal.minutes_per_day, arch[0].goal.days),
            (30, None)
        );
        // Idempotent: the flag stops a second import even if the file comes back.
        fs::write(root.join("goals.json"), LEGACY).unwrap();
        f.db.import_goals_json(&root, 10).unwrap();
        assert_eq!(f.db.goal_views(false).unwrap().len(), 1);
        assert!(root.join("goals.json").exists(), "left alone once imported");
    }

    #[test]
    fn legacy_import_creates_missing_tags_as_sun() {
        let f = fixture();
        let root = notes(&f);
        fs::write(root.join("goals.json"), LEGACY).unwrap();
        f.db.import_goals_json(&root, 9).unwrap();
        let tags = f.db.saved_tags().unwrap();
        assert_eq!(
            (tags.len(), tags[0].name.as_str(), tags[0].color.as_str()),
            (1, "mestrado", "sun")
        );
    }

    #[test]
    fn the_imported_name_is_never_overwritten() {
        let f = fixture();
        let root = notes(&f);
        fs::write(root.join("goals.json.imported"), "older").unwrap();
        fs::write(root.join("goals.json"), LEGACY).unwrap();
        f.db.import_goals_json(&root, 9).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("goals.json.imported")).unwrap(),
            "older"
        );
        assert_eq!(
            fs::read_to_string(root.join("goals.json.imported-2")).unwrap(),
            LEGACY
        );
    }

    #[test]
    fn invalid_legacy_file_is_kept_reported_and_retried() {
        let f = fixture();
        let root = notes(&f);
        for bad in [
            "{ nope".to_string(),
            r#"{"version":2,"goals":[]}"#.to_string(),
            LEGACY.replace("\"2026-10-22\"", "\"nope\""),
            LEGACY.replace("\"tag\"", "\"color\""),
        ] {
            fs::write(root.join("goals.json"), &bad).unwrap();
            let err = f.db.import_goals_json(&root, 9).unwrap_err();
            assert!(err.contains("could not be imported"), "{err}");
            assert_eq!(fs::read_to_string(root.join("goals.json")).unwrap(), bad);
            assert!(f.db.goal_views(false).unwrap().is_empty());
            assert!(f.db.goal_views(true).unwrap().is_empty(), "all or nothing");
        }
        // Fixed: the next attempt imports.
        fs::write(root.join("goals.json"), LEGACY).unwrap();
        f.db.import_goals_json(&root, 9).unwrap();
        assert_eq!(f.db.goal_views(false).unwrap().len(), 1);
    }

    #[test]
    fn no_legacy_file_means_nothing_to_do() {
        let f = fixture();
        let root = notes(&f);
        f.db.import_goals_json(&root, 9).unwrap();
        assert!(f.db.goal_views(false).unwrap().is_empty());
    }

    // ---- matching and ordering over the index

    fn session(
        start: &str,
        end: &str,
        title: &str,
        tags: &[&str],
        project: Option<&str>,
    ) -> Session {
        Session {
            start: start.into(),
            end: Some(end.into()),
            mode: Mode::Free,
            block: SessionBlock {
                start: "09:00".into(),
                end: "10:00".into(),
                title: title.into(),
                tags: tags.iter().map(|t| (*t).into()).collect(),
                project: project.map(str::to_string),
            },
            last_seen: None,
        }
    }

    fn indexed(env: &Env, sessions_in: &[(String, Session)]) -> crate::index::Index {
        for (date, s) in sessions_in {
            sessions::upsert(&env.root, date, s.clone()).unwrap();
        }
        let idx = env.open();
        idx.sync_all();
        idx
    }

    /// A session of `mins` minutes starting `hm` on `day`.
    fn sess(
        day: &str,
        hm: &str,
        mins: u32,
        title: &str,
        tags: &[&str],
        project: Option<&str>,
    ) -> (String, Session) {
        let total = hm[..2].parse::<u32>().unwrap() * 60 + hm[3..].parse::<u32>().unwrap() + mins;
        let end = format!("{:02}:{:02}", total / 60, total % 60);
        (
            day.to_string(),
            session(
                &format!("{day}T{hm}:00-03:00"),
                &format!("{day}T{end}:00-03:00"),
                title,
                tags,
                project,
            ),
        )
    }

    fn minutes(conn: &Connection, filter: &Filter) -> BTreeMap<Date, i64> {
        minutes_by_day(conn, filter).unwrap()
    }

    #[test]
    fn matching_combines_project_tags_and_title() {
        let env = Env::new();
        let rows = [
            sess(
                "2026-10-20",
                "09:00",
                30,
                "Rust  Lang",
                &["mestrado", "Mestrado"],
                Some("mest@x"),
            ),
            sess("2026-10-20", "11:00", 20, "rust lang", &[], Some("mest@x")),
            sess(
                "2026-10-20",
                "14:00",
                45,
                "Other",
                &["mestrado"],
                Some("work@x"),
            ),
            sess("2026-10-21", "09:00", 60, "Other", &["go"], None),
            sess(
                "2026-10-21",
                "12:00",
                15,
                "Other",
                &["go", "rust"],
                Some("mest@x"),
            ),
        ];
        let idx = indexed(&env, &rows);
        let conn = idx.reader().unwrap();
        let f = |c: Option<&str>, tags: &[&str], t: Option<&str>| Filter {
            calendar_id: c.map(str::to_string),
            tag_norms: tags.iter().map(|s| s.to_string()).collect(),
            title_norm: t.map(str::to_string),
        };
        // project only
        assert_eq!(
            minutes(&conn, &f(Some("mest@x"), &[], None)),
            mins(&[("2026-10-20", 50), ("2026-10-21", 15)])
        );
        // sessions without a project never match a project goal
        assert_eq!(minutes(&conn, &f(Some("none@x"), &[], None)), mins(&[]));
        // tags: ANY of them, a session with a duplicated tag counts once
        assert_eq!(
            minutes(&conn, &f(None, &["mestrado"], None)),
            mins(&[("2026-10-20", 75)])
        );
        assert_eq!(
            minutes(&conn, &f(None, &["mestrado", "go"], None)),
            mins(&[("2026-10-20", 75), ("2026-10-21", 75)])
        );
        // title only (normalised)
        assert_eq!(
            minutes(&conn, &f(None, &[], Some("rust lang"))),
            mins(&[("2026-10-20", 50)])
        );
        // project AND tags
        assert_eq!(
            minutes(&conn, &f(Some("mest@x"), &["mestrado"], None)),
            mins(&[("2026-10-20", 30)])
        );
        assert_eq!(
            minutes(&conn, &f(Some("work@x"), &["mestrado"], None)),
            mins(&[("2026-10-20", 45)])
        );
        assert_eq!(
            minutes(&conn, &f(Some("mest@x"), &["go", "rust"], None)),
            mins(&[("2026-10-21", 15)])
        );
        // project AND tags AND title
        assert_eq!(
            minutes(&conn, &f(Some("mest@x"), &["mestrado"], Some("rust lang"))),
            mins(&[("2026-10-20", 30)])
        );
        assert_eq!(
            minutes(&conn, &f(Some("mest@x"), &["go"], Some("rust lang"))),
            mins(&[])
        );
        // no filter at all matches everything (callers validate that a goal has one)
        assert_eq!(
            minutes(&conn, &f(None, &[], None)).values().sum::<i64>(),
            170
        );
    }

    #[test]
    fn running_sessions_do_not_count() {
        let env = Env::new();
        let mut running = session("2026-10-20T09:00:00-03:00", "x", "Rust", &[], None);
        running.end = None;
        let idx = indexed(&env, &[("2026-10-20".to_string(), running)]);
        let conn = idx.reader().unwrap();
        let filter = Filter {
            title_norm: Some("rust".into()),
            ..Filter::default()
        };
        assert!(minutes(&conn, &filter).is_empty());
    }

    #[test]
    fn progress_of_stored_goals_comes_from_their_filters_and_is_ordered() {
        let f = fixture();
        let env = Env::new();
        let pid = with_project(&f);
        let mk = |project: Option<i64>, tags: &[&str], start: &str, days: Option<u32>| {
            let mut d = draft();
            d.project_id = project;
            d.tag_names = tags.iter().map(|s| s.to_string()).collect();
            d.start = start.into();
            d.days = days;
            d.minutes_per_day = 30;
            f.db.goal_create(&d, 1).unwrap().id
        };
        let late = mk(None, &["rust"], "2026-10-20", None);
        let early = mk(Some(pid), &[], "2026-10-01", Some(60));
        let up = mk(None, &["rust"], "2026-10-25", Some(5));
        let fin_new = mk(None, &["rust"], "2026-10-10", Some(5));
        let fin_old = mk(None, &["rust"], "2026-09-01", Some(5));
        let rows = [sess(
            "2026-10-21",
            "09:00",
            40,
            "X",
            &["rust"],
            Some("mest@x"),
        )];
        let idx = indexed(&env, &rows);
        let views = f.db.goal_views(false).unwrap();
        let out = idx.goals(&views, d_("2026-10-21")).unwrap();
        let ids: Vec<_> = out.iter().map(|p| p.goal.id).collect();
        assert_eq!(ids, [early, late, up, fin_new, fin_old]);
        assert_eq!(
            out[0].today_minutes, 40,
            "project goal counted the mest@x session"
        );
        assert_eq!(out[0].project.as_ref().unwrap().name, "Mestrado");
        assert_eq!(out[1].today_minutes, 40, "tag goal counted it too");
        assert_eq!(out[1].tags[0].name, "rust");
        assert_eq!(out[2].today_minutes, 0);
    }
}
