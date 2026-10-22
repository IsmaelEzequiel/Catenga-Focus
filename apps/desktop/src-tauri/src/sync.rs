//! M9a: sync of the app database (tags, calendar choices, projects, goals) with the
//! account. This module is the pure/database half: the item contract, collecting
//! what changed locally, applying what the server returns (last-write-wins by
//! `changed_at`, tombstones) and the sync bookkeeping in `meta`. The HTTP half is
//! `sync_client`, the Tauri half `sync_commands`.

use crate::app_db::AppDb;
use crate::goals::check_numbers;
use crate::search::norm_tag;
use crate::tags::{self, COLORS, DEFAULT_COLOR};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const KIND_TAG: &str = "tag";
pub const KIND_CALENDAR: &str = "calendar";
pub const KIND_PROJECT: &str = "project";
pub const KIND_GOAL: &str = "goal";
/// Server limit per request.
pub const BATCH: usize = 500;

const META_USER: &str = "sync_user";

/// One synced row: `data` is a JSON object, or `null` for a tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub kind: String,
    pub key: String,
    pub data: Option<Value>,
    pub changed_at: i64,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRef {
    pub kind: String,
    pub key: String,
}

/// Which kinds of local data a pull changed (to emit the `*-changed` events).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    pub tags: bool,
    pub calendars: bool,
    pub projects: bool,
    pub goals: bool,
}

impl Applied {
    #[cfg(test)]
    pub fn any(self) -> bool {
        self.tags || self.calendars || self.projects || self.goals
    }

    pub fn merge(&mut self, other: Applied) {
        self.tags |= other.tags;
        self.calendars |= other.calendars;
        self.projects |= other.projects;
        self.goals |= other.goals;
    }
}

fn rank(kind: &str) -> u8 {
    match kind {
        KIND_TAG => 0,
        KIND_CALENDAR => 1,
        KIND_PROJECT => 2,
        _ => 3,
    }
}

fn cursor_key(user: &str) -> String {
    format!("sync_cursor:{user}")
}

fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map(drop)
}

// ------------------------------------------------------------------ collecting

fn collect_dirty(conn: &Connection) -> rusqlite::Result<Vec<Item>> {
    let mut out = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT name, name_norm, color, changed_at, deleted_at IS NOT NULL
         FROM tags WHERE dirty = 1 ORDER BY id",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })? {
        let (name, key, color, changed_at, deleted) = row?;
        out.push(Item {
            kind: KIND_TAG.into(),
            key,
            data: (!deleted).then(|| json!({ "name": name, "color": color })),
            changed_at,
            deleted,
        });
    }

    let mut stmt = conn.prepare(
        "SELECT google_calendar_id, show_on_timeline, changed_at
         FROM calendars WHERE dirty = 1 ORDER BY id",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, bool>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (key, shown, changed_at) = row?;
        out.push(Item {
            kind: KIND_CALENDAR.into(),
            key,
            data: Some(json!({ "showOnTimeline": shown })),
            changed_at,
            deleted: false,
        });
    }

    let mut stmt = conn.prepare(
        "SELECT kind, google_id, calendar_id, name, archived, changed_at
         FROM projects WHERE dirty = 1 ORDER BY id",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })? {
        let (kind, google_id, calendar_id, name, archived, changed_at) = row?;
        out.push(Item {
            kind: KIND_PROJECT.into(),
            key: format!("{kind}:{google_id}"),
            data: Some(json!({
                "kind": kind, "googleId": google_id, "calendarId": calendar_id,
                "name": name, "archived": archived
            })),
            changed_at,
            deleted: false,
        });
    }

    let mut tag_names = conn.prepare(
        "SELECT t.name FROM goal_tags g JOIN tags t ON t.id = g.tag_id
         WHERE g.goal_id = ?1 ORDER BY t.name_norm",
    )?;
    let mut stmt = conn.prepare(
        "SELECT g.id, g.uid,
                COALESCE((SELECT p.kind || ':' || p.google_id FROM projects p
                           WHERE p.id = g.project_id), g.pending_project_key),
                g.title, g.minutes_per_day, g.start, g.days, g.archived,
                g.changed_at, g.deleted_at IS NOT NULL
         FROM goals g WHERE g.dirty = 1 AND g.uid IS NOT NULL ORDER BY g.id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, bool>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, uid, project, title, minutes, start, days, archived, changed_at, deleted) in rows {
        let data = if deleted {
            None
        } else {
            let names = tag_names
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Some(json!({
                "projectKey": project, "tagNames": names, "title": title,
                "minutesPerDay": minutes, "start": start, "days": days,
                "archived": archived
            }))
        };
        out.push(Item {
            kind: KIND_GOAL.into(),
            key: uid,
            data,
            changed_at,
            deleted,
        });
    }
    Ok(out)
}

// -------------------------------------------------------------------- applying

fn secs(ms: i64) -> i64 {
    ms / 1000
}

enum Verdict {
    /// Remote is strictly newer (or the row is missing): apply it.
    Apply,
    /// Same version: nothing to rewrite; the row is in sync.
    Tie,
    /// Local is newer: keep it (it stays dirty and is pushed).
    Keep,
}

fn verdict(local: Option<i64>, remote: i64) -> Verdict {
    match local {
        None => Verdict::Apply,
        Some(l) if remote > l => Verdict::Apply,
        Some(l) if remote == l => Verdict::Tie,
        Some(_) => Verdict::Keep,
    }
}

fn str_of<'a>(data: &'a Value, field: &str) -> Option<&'a str> {
    data.get(field)?.as_str()
}

fn apply_tag(tx: &Connection, it: &Item) -> rusqlite::Result<bool> {
    let name_in = it
        .data
        .as_ref()
        .and_then(|d| str_of(d, "name"))
        .map(str::to_string);
    let key = it.key.clone();
    let local: Option<(i64, i64, bool, String, String)> = tx
        .query_row(
            "SELECT id, changed_at, deleted_at IS NOT NULL, name, color
             FROM tags WHERE name_norm = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let v = verdict(local.as_ref().map(|l| l.1), it.changed_at);
    if matches!(v, Verdict::Tie) {
        tx.execute(
            "UPDATE tags SET dirty = 0 WHERE name_norm = ?1 AND changed_at = ?2",
            params![key, it.changed_at],
        )?;
        return Ok(false);
    }
    if matches!(v, Verdict::Keep) {
        return Ok(false);
    }
    if it.deleted {
        let Some((id, _, was_deleted, ..)) = local else {
            return Ok(false);
        };
        let used: i64 = tx.query_row(
            "SELECT count(*) FROM goal_tags WHERE tag_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        if !was_deleted && used > 0 {
            // A live goal still uses it: keep the tag and tell the server so.
            tx.execute(
                "UPDATE tags SET changed_at = ?2, dirty = 1 WHERE id = ?1",
                params![id, it.changed_at + 1],
            )?;
            return Ok(false);
        }
        tx.execute(
            "UPDATE tags SET deleted_at = COALESCE(deleted_at, ?2), changed_at = ?2, dirty = 0
             WHERE id = ?1",
            params![id, it.changed_at],
        )?;
        return Ok(!was_deleted);
    }
    let Some(name) = name_in.and_then(|n| tags::clean_name(&n).ok()) else {
        return Ok(false);
    };
    if norm_tag(&name) != key {
        return Ok(false);
    }
    let color = it
        .data
        .as_ref()
        .and_then(|d| str_of(d, "color"))
        .filter(|c| COLORS.contains(c))
        .unwrap_or(DEFAULT_COLOR)
        .to_string();
    match local {
        Some((id, _, was_deleted, old_name, old_color)) => {
            tx.execute(
                "UPDATE tags SET name = ?2, color = ?3, deleted_at = NULL, changed_at = ?4,
                                 dirty = 0 WHERE id = ?1",
                params![id, name, color, it.changed_at],
            )?;
            Ok(was_deleted || old_name != name || old_color != color)
        }
        None => {
            tx.execute(
                "INSERT INTO tags(name, name_norm, color, created_at, changed_at, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![name, key, color, secs(it.changed_at), it.changed_at],
            )?;
            Ok(true)
        }
    }
}

fn apply_calendar(tx: &Connection, it: &Item) -> rusqlite::Result<bool> {
    if it.deleted {
        return Ok(false);
    }
    let Some(shown) = it
        .data
        .as_ref()
        .and_then(|d| d.get("showOnTimeline"))
        .and_then(Value::as_bool)
    else {
        return Ok(false);
    };
    let local: Option<(i64, i64, bool)> = tx
        .query_row(
            "SELECT id, changed_at, show_on_timeline FROM calendars WHERE google_calendar_id = ?1",
            [&it.key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match verdict(local.map(|l| l.1), it.changed_at) {
        Verdict::Keep => Ok(false),
        Verdict::Tie => {
            tx.execute(
                "UPDATE calendars SET dirty = 0 WHERE google_calendar_id = ?1 AND changed_at = ?2",
                params![it.key, it.changed_at],
            )?;
            Ok(false)
        }
        Verdict::Apply => match local {
            Some((id, _, old)) => {
                tx.execute(
                    "UPDATE calendars SET show_on_timeline = ?2, changed_at = ?3, dirty = 0
                     WHERE id = ?1",
                    params![id, shown, it.changed_at],
                )?;
                Ok(old != shown)
            }
            None => {
                // Not seen from Google on this computer yet: keep the choice in an
                // archived placeholder; the next calendar refresh un-archives and
                // names it without touching `show_on_timeline`.
                tx.execute(
                    "INSERT INTO calendars(google_calendar_id, name, color, show_on_timeline,
                                           is_primary, archived, updated_at, changed_at, dirty)
                     VALUES (?1, ?1, ?2, ?3, 0, 1, ?4, ?5, 0)",
                    params![
                        it.key,
                        crate::google::DEFAULT_CALENDAR_COLOR,
                        shown,
                        secs(it.changed_at),
                        it.changed_at
                    ],
                )?;
                Ok(true)
            }
        },
    }
}

fn apply_project(tx: &Connection, it: &Item) -> rusqlite::Result<bool> {
    let Some(d) = it.data.as_ref().filter(|_| !it.deleted) else {
        return Ok(false);
    };
    let (Some(kind), Some(google_id), Some(calendar_id), Some(name), Some(archived)) = (
        str_of(d, "kind").filter(|k| matches!(*k, "series" | "event")),
        str_of(d, "googleId"),
        str_of(d, "calendarId"),
        str_of(d, "name"),
        d.get("archived").and_then(Value::as_bool),
    ) else {
        return Ok(false);
    };
    if it.key != format!("{kind}:{google_id}") {
        return Ok(false);
    }
    let local: Option<(i64, i64, bool, String)> = tx
        .query_row(
            "SELECT id, changed_at, archived, name FROM projects WHERE kind = ?1 AND google_id = ?2",
            params![kind, google_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    match verdict(local.as_ref().map(|l| l.1), it.changed_at) {
        Verdict::Keep => Ok(false),
        Verdict::Tie => {
            tx.execute(
                "UPDATE projects SET dirty = 0 WHERE kind = ?1 AND google_id = ?2 AND changed_at = ?3",
                params![kind, google_id, it.changed_at],
            )?;
            Ok(false)
        }
        Verdict::Apply => match local {
            Some((id, _, old_archived, old_name)) => {
                // Google owns a series' name and calendar; only a one-off event project's are the user's.
                if kind == "event" {
                    tx.execute(
                        "UPDATE projects SET name = ?2, calendar_id = ?3 WHERE id = ?1",
                        params![id, name, calendar_id],
                    )?;
                }
                tx.execute(
                    "UPDATE projects SET archived = ?2, changed_at = ?3, dirty = 0 WHERE id = ?1",
                    params![id, archived, it.changed_at],
                )?;
                Ok(old_archived != archived || (kind == "event" && old_name != name))
            }
            None => {
                tx.execute(
                    "INSERT INTO projects(kind, google_id, calendar_id, name, archived,
                                          updated_at, changed_at, dirty)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                    params![
                        kind,
                        google_id,
                        calendar_id,
                        name,
                        archived,
                        secs(it.changed_at),
                        it.changed_at
                    ],
                )?;
                Ok(true)
            }
        },
    }
}

/// The id of a live tag with this name; unknown names are created as saved (color
/// `sun`, version 0 so any real tag item wins); a deleted one is brought back and
/// re-announced (a live goal uses it).
fn tag_for_goal(tx: &Connection, name: &str) -> rusqlite::Result<i64> {
    let key = norm_tag(name);
    let found: Option<(i64, i64, bool)> = tx
        .query_row(
            "SELECT id, changed_at, deleted_at IS NOT NULL FROM tags WHERE name_norm = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match found {
        Some((id, _, false)) => Ok(id),
        Some((id, changed_at, true)) => {
            tx.execute(
                "UPDATE tags SET deleted_at = NULL, changed_at = ?2, dirty = 1 WHERE id = ?1",
                params![id, changed_at + 1],
            )?;
            Ok(id)
        }
        None => {
            tx.execute(
                "INSERT INTO tags(name, name_norm, color, created_at, changed_at, dirty)
                 VALUES (?1, ?2, ?3, 0, 0, 0)",
                params![name, key, DEFAULT_COLOR],
            )?;
            Ok(tx.last_insert_rowid())
        }
    }
}

struct GoalData {
    project_key: Option<String>,
    tag_names: Vec<String>,
    title: Option<String>,
    minutes: u32,
    start: String,
    days: Option<u32>,
    archived: bool,
}

fn parse_goal(d: &Value) -> Option<GoalData> {
    let minutes = u32::try_from(d.get("minutesPerDay")?.as_u64()?).ok()?;
    let start = str_of(d, "start")?.to_string();
    let days = match d.get("days") {
        None | Some(Value::Null) => None,
        Some(v) => Some(u32::try_from(v.as_u64()?).ok()?),
    };
    check_numbers(minutes, days, &start).ok()?;
    let project_key = match d.get("projectKey") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str()?.to_string()),
    };
    let mut tag_names = Vec::new();
    for v in d.get("tagNames")?.as_array()? {
        tag_names.push(tags::clean_name(v.as_str()?).ok()?);
    }
    let title = match d.get("title") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str()?.to_string()),
    };
    Some(GoalData {
        project_key,
        tag_names,
        title,
        minutes,
        start,
        days,
        archived: d.get("archived")?.as_bool()?,
    })
}

fn apply_goal(tx: &Connection, it: &Item) -> rusqlite::Result<bool> {
    let local: Option<(i64, i64, bool)> = tx
        .query_row(
            "SELECT id, changed_at, deleted_at IS NOT NULL FROM goals WHERE uid = ?1",
            [&it.key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match verdict(local.map(|l| l.1), it.changed_at) {
        Verdict::Keep => return Ok(false),
        Verdict::Tie => {
            tx.execute(
                "UPDATE goals SET dirty = 0 WHERE uid = ?1 AND changed_at = ?2",
                params![it.key, it.changed_at],
            )?;
            return Ok(false);
        }
        Verdict::Apply => {}
    }
    if it.deleted {
        let Some((id, _, was_deleted)) = local else {
            return Ok(false);
        };
        tx.execute(
            "UPDATE goals SET deleted_at = COALESCE(deleted_at, ?2), changed_at = ?2, dirty = 0,
                              project_id = NULL, pending_project_key = NULL, title = NULL
             WHERE id = ?1",
            params![id, it.changed_at],
        )?;
        tx.execute("DELETE FROM goal_tags WHERE goal_id = ?1", [id])?;
        return Ok(!was_deleted);
    }
    let Some(g) = it.data.as_ref().and_then(parse_goal) else {
        return Ok(false);
    };
    let (project_id, pending): (Option<i64>, Option<String>) = match &g.project_key {
        None => (None, None),
        Some(key) => {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM projects WHERE kind || ':' || google_id = ?1",
                    [key],
                    |r| r.get(0),
                )
                .optional()?;
            (id, id.is_none().then(|| key.clone()))
        }
    };
    let id = match local {
        Some((id, ..)) => {
            tx.execute(
                "UPDATE goals SET project_id = ?2, pending_project_key = ?3, title = ?4,
                                  minutes_per_day = ?5, start = ?6, days = ?7, archived = ?8,
                                  deleted_at = NULL, changed_at = ?9, dirty = 0, updated_at = ?10
                 WHERE id = ?1",
                params![
                    id,
                    project_id,
                    pending,
                    g.title,
                    g.minutes,
                    g.start,
                    g.days,
                    g.archived,
                    it.changed_at,
                    secs(it.changed_at)
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO goals(project_id, pending_project_key, title, minutes_per_day, start,
                                   days, archived, created_at, updated_at, uid, changed_at, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, 0)",
                params![
                    project_id,
                    pending,
                    g.title,
                    g.minutes,
                    g.start,
                    g.days,
                    g.archived,
                    secs(it.changed_at),
                    it.key,
                    it.changed_at
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.execute("DELETE FROM goal_tags WHERE goal_id = ?1", [id])?;
    for name in &g.tag_names {
        let tag_id = tag_for_goal(tx, name)?;
        tx.execute(
            "INSERT OR IGNORE INTO goal_tags(goal_id, tag_id) VALUES (?1, ?2)",
            params![id, tag_id],
        )?;
    }
    Ok(true)
}

// ------------------------------------------------------------------- AppDb API

impl AppDb {
    /// Starts a sync for `user` and returns the pull cursor. When the local data was last
    /// synced with another account (or never fully synced), everything local is marked
    /// changed so the first sync pushes it all.
    pub fn sync_begin(&self, user: &str) -> Result<i64, String> {
        self.with_conn("preparing sync", |conn| {
            let tx = conn.transaction()?;
            let cursor = meta_get(&tx, &cursor_key(user))?.and_then(|c| c.parse::<i64>().ok());
            let same_user = meta_get(&tx, META_USER)?.as_deref() == Some(user);
            if !same_user || cursor.is_none() {
                tx.execute_batch(
                    "UPDATE tags SET dirty = 1;
                     UPDATE goals SET dirty = 1;
                     UPDATE projects SET dirty = 1 WHERE changed_at > 0;
                     UPDATE calendars SET dirty = 1 WHERE changed_at > 0;",
                )?;
                meta_set(&tx, META_USER, user)?;
            }
            tx.commit()?;
            Ok(cursor.unwrap_or(0))
        })
    }

    pub fn sync_save_cursor(&self, user: &str, cursor: i64) -> Result<(), String> {
        self.with_conn("saving sync cursor", |conn| {
            meta_set(conn, &cursor_key(user), &cursor.to_string())
        })
    }

    /// Up to `BATCH` changed rows, tags first, goals last.
    pub fn sync_collect(&self) -> Result<Vec<Item>, String> {
        let mut items = self.with_conn("collecting changes", |conn| collect_dirty(conn))?;
        items.sort_by_key(|i| rank(&i.kind));
        items.truncate(BATCH);
        Ok(items)
    }

    /// Marks the accepted rows as synced, unless the user changed them meanwhile.
    pub fn sync_ack(&self, pushed: &[Item], accepted: &[KeyRef]) -> Result<(), String> {
        self.with_conn("saving sync result", |conn| {
            let tx = conn.transaction()?;
            for a in accepted {
                let Some(p) = pushed.iter().find(|p| p.kind == a.kind && p.key == a.key) else {
                    continue;
                };
                let (sql, key) = match p.kind.as_str() {
                    KIND_TAG => ("UPDATE tags SET dirty = 0 WHERE name_norm = ?1 AND changed_at = ?2", &p.key),
                    KIND_CALENDAR => ("UPDATE calendars SET dirty = 0 WHERE google_calendar_id = ?1 AND changed_at = ?2", &p.key),
                    KIND_PROJECT => ("UPDATE projects SET dirty = 0 WHERE kind || ':' || google_id = ?1 AND changed_at = ?2", &p.key),
                    KIND_GOAL => ("UPDATE goals SET dirty = 0 WHERE uid = ?1 AND changed_at = ?2", &p.key),
                    _ => continue,
                };
                tx.execute(sql, params![key, p.changed_at])?;
            }
            tx.commit()
        })
    }

    /// Applies pulled items (tags, calendars, projects, goals, in that order) with
    /// last-write-wins against the local `changed_at`.
    pub fn sync_apply(&self, mut items: Vec<Item>) -> Result<Applied, String> {
        items.sort_by_key(|i| rank(&i.kind));
        self.with_conn("applying synced changes", |conn| {
            let tx = conn.transaction()?;
            let mut applied = Applied::default();
            for it in &items {
                match it.kind.as_str() {
                    KIND_TAG => applied.tags |= apply_tag(&tx, it)?,
                    KIND_CALENDAR => applied.calendars |= apply_calendar(&tx, it)?,
                    KIND_PROJECT => applied.projects |= apply_project(&tx, it)?,
                    KIND_GOAL => applied.goals |= apply_goal(&tx, it)?,
                    _ => {}
                }
            }
            tx.commit()?;
            Ok(applied)
        })
    }

    /// Points goals that arrived before their project at the project, now that it is known.
    pub fn resolve_pending_projects(&self) -> Result<bool, String> {
        self.with_conn("resolving goal projects", |conn| {
            conn.execute(
                "UPDATE goals SET
                   project_id = (SELECT p.id FROM projects p
                                  WHERE p.kind || ':' || p.google_id = goals.pending_project_key),
                   pending_project_key = NULL
                 WHERE pending_project_key IS NOT NULL
                   AND EXISTS (SELECT 1 FROM projects p
                                WHERE p.kind || ':' || p.google_id = goals.pending_project_key)",
                [],
            )
            .map(|n| n > 0)
        })
    }

    /// Rows changed locally and not synced yet.
    pub fn sync_pending(&self) -> Result<i64, String> {
        self.with_conn("counting changes", |conn| {
            conn.query_row(
                "SELECT (SELECT count(*) FROM tags WHERE dirty = 1)
                      + (SELECT count(*) FROM calendars WHERE dirty = 1)
                      + (SELECT count(*) FROM projects WHERE dirty = 1)
                      + (SELECT count(*) FROM goals WHERE dirty = 1 AND uid IS NOT NULL)",
                [],
                |r| r.get(0),
            )
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app_db::test_util::{add_series, cal};
    use crate::config::test_util::TempDir;
    use crate::goals::GoalDraft;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A database whose clock is a shared counter (each read is a later millisecond).
    pub struct Dev {
        pub _tmp: TempDir,
        pub db: AppDb,
        pub path: PathBuf,
    }

    pub fn dev(clock: &Arc<AtomicI64>) -> Dev {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        let c = clock.clone();
        let db = AppDb::open_with_clock(&path, move || c.fetch_add(10, Ordering::SeqCst)).unwrap();
        Dev {
            _tmp: tmp,
            db,
            path,
        }
    }

    pub fn clock() -> Arc<AtomicI64> {
        Arc::new(AtomicI64::new(1_000_000))
    }

    pub fn draft(title: &str, tags: &[&str], project: Option<i64>) -> GoalDraft {
        GoalDraft {
            project_id: project,
            tag_names: tags.iter().map(|t| (*t).to_string()).collect(),
            title: Some(title.to_string()).filter(|t| !t.is_empty()),
            minutes_per_day: 60,
            start: "2026-10-01".into(),
            days: Some(30),
        }
    }

    fn item(kind: &str, key: &str, data: Value, at: i64) -> Item {
        Item {
            kind: kind.into(),
            key: key.into(),
            data: Some(data),
            changed_at: at,
            deleted: false,
        }
    }

    fn tomb(kind: &str, key: &str, at: i64) -> Item {
        Item {
            kind: kind.into(),
            key: key.into(),
            data: None,
            changed_at: at,
            deleted: true,
        }
    }

    fn goal_item(uid: &str, title: &str, at: i64) -> Item {
        item(
            KIND_GOAL,
            uid,
            json!({"projectKey": null, "tagNames": [], "title": title, "minutesPerDay": 30,
                   "start": "2026-10-01", "days": null, "archived": false}),
            at,
        )
    }

    fn titles(db: &AppDb) -> Vec<String> {
        db.goal_views(false)
            .unwrap()
            .into_iter()
            .filter_map(|v| v.goal.title)
            .collect()
    }

    #[test]
    fn user_writes_stamp_changed_at_and_dirty_and_goals_get_a_uuid() {
        let c = clock();
        let d = dev(&c);
        d.db.goal_create(&draft("Rust", &["rust"], None), 5)
            .unwrap();
        let items = d.db.sync_collect().unwrap();
        let kinds: Vec<_> = items.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds, ["tag", "goal"], "tags first");
        let goal = &items[1];
        assert_eq!(goal.key.len(), 36);
        assert_eq!(goal.key.as_bytes()[14], b'4', "version 4");
        assert!(matches!(goal.key.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert!(goal.changed_at >= 1_000_000);
        assert_eq!(
            goal.data,
            Some(
                json!({"projectKey": null, "tagNames": ["rust"], "title": "Rust",
                        "minutesPerDay": 60, "start": "2026-10-01", "days": 30,
                        "archived": false})
            )
        );
        assert_eq!(items[0].data, Some(json!({"name": "rust", "color": "sun"})));
        // A raw insert (no uid given) still gets one from the trigger.
        d.db.with_conn("t", |c| {
            c.execute(
                "INSERT INTO goals(minutes_per_day, start, created_at, updated_at) VALUES (60,'2026-10-21',1,1)",
                [],
            )
        })
        .unwrap();
        let uids: Vec<_> =
            d.db.sync_collect()
                .unwrap()
                .into_iter()
                .filter(|i| i.kind == "goal")
                .map(|i| i.key)
                .collect();
        assert_eq!(uids.len(), 2);
        assert_ne!(uids[0], uids[1]);
    }

    #[test]
    fn changed_at_always_moves_forward_even_if_the_clock_does_not() {
        let d = dev(&Arc::new(AtomicI64::new(0)));
        let frozen = AppDb::open_with_clock(&d.path.with_file_name("b.sqlite"), || 5_000).unwrap();
        frozen.tag_save("a", "sun", 1).unwrap();
        let a = frozen.sync_collect().unwrap()[0].changed_at;
        frozen.tag_save("a", "sky", 1).unwrap();
        let b = frozen.sync_collect().unwrap()[0].changed_at;
        assert!(b > a);
    }

    #[test]
    fn ack_clears_dirty_only_when_unchanged_since() {
        let c = clock();
        let d = dev(&c);
        d.db.tag_save("a", "sun", 1).unwrap();
        d.db.tag_save("b", "sun", 1).unwrap();
        let pushed = d.db.sync_collect().unwrap();
        // "a" is edited while the request is in flight.
        d.db.tag_save("a", "leaf", 1).unwrap();
        let accepted: Vec<_> = pushed
            .iter()
            .map(|p| KeyRef {
                kind: p.kind.clone(),
                key: p.key.clone(),
            })
            .collect();
        d.db.sync_ack(&pushed, &accepted).unwrap();
        let left = d.db.sync_collect().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key, "a");
        assert_eq!(d.db.sync_pending().unwrap(), 1);
    }

    #[test]
    fn rejected_items_stay_dirty() {
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        let pushed = d.db.sync_collect().unwrap();
        d.db.sync_ack(&pushed, &[]).unwrap();
        assert_eq!(d.db.sync_pending().unwrap(), 1);
    }

    #[test]
    fn pull_applies_newer_ignores_older_and_ties_are_noops() {
        let d = dev(&clock());
        d.db.goal_create(&draft("Local", &[], None), 1).unwrap();
        let uid = d.db.sync_collect().unwrap()[0].key.clone();
        let at = d.db.sync_collect().unwrap()[0].changed_at;
        // Older: ignored, stays dirty.
        let r =
            d.db.sync_apply(vec![goal_item(&uid, "Old", at - 1)])
                .unwrap();
        assert!(!r.any());
        assert_eq!(titles(&d.db), ["Local"]);
        assert_eq!(d.db.sync_pending().unwrap(), 1);
        // Tie: no rewrite, no event, row is in sync.
        let r = d.db.sync_apply(vec![goal_item(&uid, "Tie", at)]).unwrap();
        assert!(!r.any());
        assert_eq!(titles(&d.db), ["Local"]);
        assert_eq!(d.db.sync_pending().unwrap(), 0);
        // Newer: applied, clean.
        let r =
            d.db.sync_apply(vec![goal_item(&uid, "New", at + 1)])
                .unwrap();
        assert!(r.goals);
        assert_eq!(titles(&d.db), ["New"]);
        assert_eq!(d.db.sync_pending().unwrap(), 0);
    }

    #[test]
    fn tombstones_hide_goals_and_tags_and_unknown_ones_are_ignored() {
        let d = dev(&clock());
        d.db.goal_create(&draft("Gone", &[], None), 1).unwrap();
        d.db.tag_save("old", "sky", 1).unwrap();
        let uid =
            d.db.sync_collect()
                .unwrap()
                .into_iter()
                .find(|i| i.kind == "goal")
                .unwrap()
                .key;
        let r =
            d.db.sync_apply(vec![
                tomb(KIND_GOAL, &uid, 9_999_999),
                tomb(KIND_TAG, "old", 9_999_999),
                tomb(KIND_GOAL, "nope", 9_999_999),
                tomb(KIND_TAG, "nope", 9_999_999),
            ])
            .unwrap();
        assert!(r.goals && r.tags);
        assert!(titles(&d.db).is_empty());
        assert!(d.db.goal_views(true).unwrap().is_empty());
        assert!(d.db.saved_tags().unwrap().is_empty());
        assert_eq!(d.db.sync_pending().unwrap(), 0);
    }

    #[test]
    fn local_delete_is_a_tombstone_that_syncs_and_can_be_edited_never() {
        let d = dev(&clock());
        d.db.goal_create(&draft("Bye", &["x"], None), 1).unwrap();
        let id = d.db.goal_views(false).unwrap()[0].goal.id;
        d.db.goal_delete(id).unwrap();
        assert!(titles(&d.db).is_empty());
        assert_eq!(d.db.goal_delete(id), Err("Goal not found".into()));
        assert_eq!(
            d.db.goal_edit(id, &draft("Zombie", &[], None), 1),
            Err("Goal not found".into())
        );
        let t =
            d.db.sync_collect()
                .unwrap()
                .into_iter()
                .find(|i| i.kind == "goal")
                .unwrap();
        assert!(t.deleted && t.data.is_none());
        // Its tag is free to delete now.
        let tag = d.db.saved_tags().unwrap()[0].id.unwrap();
        d.db.tag_delete(tag).unwrap();
        let tags: Vec<_> =
            d.db.sync_collect()
                .unwrap()
                .into_iter()
                .filter(|i| i.kind == "tag")
                .collect();
        assert!(tags[0].deleted);
        // Used again, the tombstoned tag comes back (and is announced).
        d.db.tag_save("x", "leaf", 1).unwrap();
        assert_eq!(d.db.saved_tags().unwrap()[0].color, "leaf");
    }

    #[test]
    fn remote_tag_delete_keeps_a_tag_a_live_goal_uses_and_reasserts_it() {
        let d = dev(&clock());
        d.db.goal_create(&draft("G", &["rust"], None), 1).unwrap();
        let r =
            d.db.sync_apply(vec![tomb(KIND_TAG, "rust", 99_999_999)])
                .unwrap();
        assert!(!r.tags);
        assert_eq!(d.db.saved_tags().unwrap().len(), 1);
        let again =
            d.db.sync_collect()
                .unwrap()
                .into_iter()
                .find(|i| i.kind == "tag")
                .unwrap();
        assert!(!again.deleted && again.changed_at > 99_999_999);
    }

    #[test]
    fn goal_with_an_unknown_project_is_kept_pending_then_resolved() {
        let d = dev(&clock());
        let it = item(
            KIND_GOAL,
            "11111111-1111-4111-8111-111111111111",
            json!({"projectKey": "series:abc", "tagNames": ["new"], "title": null,
                   "minutesPerDay": 45, "start": "2026-10-01", "days": null, "archived": false}),
            5_000,
        );
        d.db.sync_apply(vec![it.clone()]).unwrap();
        let v = &d.db.goal_views(false).unwrap()[0];
        assert!(v.project.is_none());
        assert_eq!(v.pending_project_key.as_deref(), Some("series:abc"));
        assert_eq!(
            v.filter().project_key.as_deref(),
            Some("series:abc"),
            "still counts its sessions"
        );
        assert_eq!(v.tags[0].name, "new", "unknown tags are created as saved");
        assert_eq!(v.tags[0].color, "sun");
        assert!(!d.db.resolve_pending_projects().unwrap());
        // The project shows up (Google refresh).
        add_series(&d.db, "me@x", "abc", "Learn Rust");
        assert!(d.db.resolve_pending_projects().unwrap());
        let v = &d.db.goal_views(false).unwrap()[0];
        assert_eq!(v.project.as_ref().unwrap().project.key, "series:abc");
        assert_eq!(v.pending_project_key, None);
        // And it is not dirty because of that.
        assert_eq!(d.db.sync_pending().unwrap(), 0);
    }

    #[test]
    fn project_calendar_and_tag_items_apply_and_create_missing_rows() {
        let d = dev(&clock());
        d.db.sync_apply(vec![
            item(KIND_CALENDAR, "mest@x", json!({"showOnTimeline": false}), 100),
            item(
                KIND_PROJECT,
                "event:e1",
                json!({"kind":"event","googleId":"e1","calendarId":"mest@x","name":"Dentist","archived":false}),
                100,
            ),
            item(KIND_TAG, "rust", json!({"name":"Rust","color":"clay"}), 100),
            // Wrong key for its data: skipped.
            item(
                KIND_PROJECT,
                "series:zzz",
                json!({"kind":"series","googleId":"other","calendarId":"c","name":"x","archived":false}),
                100,
            ),
            // Bad color falls back to sun; bad name skipped.
            item(KIND_TAG, "go", json!({"name":"go","color":"magenta"}), 100),
            item(KIND_TAG, "bad", json!({"name":"two words","color":"sun"}), 100),
        ])
        .unwrap();
        let cals = d.db.calendars().unwrap();
        assert_eq!(cals.len(), 1);
        assert!(!cals[0].show_on_timeline && cals[0].archived);
        // The next Google refresh names and un-archives it, keeping the choice.
        d.db.upsert_calendars(&[cal("mest@x", "Mestrado", false, true)], 3)
            .unwrap();
        let c = &d.db.calendars().unwrap()[0];
        assert_eq!(
            (c.name.as_str(), c.show_on_timeline, c.archived),
            ("Mestrado", false, false)
        );
        assert_eq!(d.db.projects().unwrap()[0].name, "Dentist");
        let tags = d.db.saved_tags().unwrap();
        let by: Vec<_> = tags
            .iter()
            .map(|t| (t.name.as_str(), t.color.as_str()))
            .collect();
        assert_eq!(by, [("go", "sun"), ("Rust", "clay")]);
        assert_eq!(d.db.sync_pending().unwrap(), 0, "applied rows are clean");
    }

    #[test]
    fn begin_marks_everything_dirty_for_a_new_account_or_cursor() {
        let d = dev(&clock());
        d.db.goal_create(&draft("G", &["t"], None), 1).unwrap();
        d.db.tag_save("t", "sky", 1).unwrap();
        add_series(&d.db, "a@x", "s", "Series"); // untouched by the user
        d.db.upsert_calendars(&[cal("a@x", "A", true, true)], 1)
            .unwrap();
        // Everything is acked as if synced with user 1.
        let all = d.db.sync_collect().unwrap();
        let acc: Vec<_> = all
            .iter()
            .map(|p| KeyRef {
                kind: p.kind.clone(),
                key: p.key.clone(),
            })
            .collect();
        d.db.sync_ack(&all, &acc).unwrap();
        assert_eq!(d.db.sync_pending().unwrap(), 0);
        assert_eq!(d.db.sync_begin("u1").unwrap(), 0);
        assert_eq!(
            d.db.sync_pending().unwrap(),
            2,
            "tag + goal; untouched project/calendar are not user data"
        );
        let all = d.db.sync_collect().unwrap();
        let acc: Vec<_> = all
            .iter()
            .map(|p| KeyRef {
                kind: p.kind.clone(),
                key: p.key.clone(),
            })
            .collect();
        d.db.sync_ack(&all, &acc).unwrap();
        d.db.sync_save_cursor("u1", 42).unwrap();
        // Same user, has cursor: nothing is re-marked.
        assert_eq!(d.db.sync_begin("u1").unwrap(), 42);
        assert_eq!(d.db.sync_pending().unwrap(), 0);
        // A different account: everything goes into it, from cursor 0.
        assert_eq!(d.db.sync_begin("u2").unwrap(), 0);
        assert_eq!(d.db.sync_pending().unwrap(), 2);
        // Back to the first one: its cursor is still there, but data is pushed again.
        assert_eq!(d.db.sync_begin("u1").unwrap(), 42);
        assert_eq!(d.db.sync_pending().unwrap(), 2);
    }

    #[test]
    fn user_choices_on_projects_and_calendars_become_items() {
        let d = dev(&clock());
        d.db.upsert_calendars(&[cal("a@x", "A", true, true)], 1)
            .unwrap();
        let p = add_series(&d.db, "a@x", "s", "Series");
        d.db.project_from_event("a@x", "ev", "Dentist", 2).unwrap();
        assert_eq!(
            d.db.sync_pending().unwrap(),
            1,
            "only the one-off project so far"
        );
        d.db.project_set_archived(p, true, 3).unwrap();
        d.db.set_shown(d.db.calendars().unwrap()[0].id, false, 3)
            .unwrap();
        let items = d.db.sync_collect().unwrap();
        let keys: Vec<_> = items
            .iter()
            .map(|i| (i.kind.as_str(), i.key.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("calendar", "a@x"),
                ("project", "series:s"),
                ("project", "event:ev")
            ]
        );
        assert_eq!(items[0].data, Some(json!({"showOnTimeline": false})));
        assert_eq!(
            items[1].data,
            Some(
                json!({"kind":"series","googleId":"s","calendarId":"a@x","name":"Series","archived":true})
            )
        );
    }

    #[test]
    fn two_computers_converge_with_different_local_ids() {
        let c = clock();
        let (a, b) = (dev(&c), dev(&c));
        // B has an unrelated goal first so local ids differ.
        b.db.goal_create(&draft("B only", &[], None), 1).unwrap();
        a.db.goal_create(&draft("From A", &["rust"], None), 1)
            .unwrap();
        let from_a = a.db.sync_collect().unwrap();
        b.db.sync_apply(from_a.clone()).unwrap();
        let ids = |d: &Dev| {
            d.db.goal_views(false)
                .unwrap()
                .into_iter()
                .map(|v| (v.goal.id, v.goal.title.unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&a), [(1, "From A".to_string())]);
        assert_eq!(
            ids(&b),
            [(1, "B only".to_string()), (2, "From A".to_string())]
        );
        // B edits it later; A takes the edit.
        let b_id = b.db.goal_views(false).unwrap()[1].goal.id;
        b.db.goal_edit(b_id, &draft("Edited on B", &["rust", "go"], None), 2)
            .unwrap();
        let from_b: Vec<_> =
            b.db.sync_collect()
                .unwrap()
                .into_iter()
                .filter(|i| i.kind == "goal" && i.data.as_ref().unwrap()["title"] == "Edited on B")
                .collect();
        a.db.sync_apply(from_b).unwrap();
        let v = &a.db.goal_views(false).unwrap()[0];
        assert_eq!(v.goal.title.as_deref(), Some("Edited on B"));
        let mut names: Vec<_> = v.tags.iter().map(|t| t.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["go", "rust"]);
    }
}
