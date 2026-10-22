//! `catenga.sqlite`: the app database, a SOURCE OF TRUTH for calendars, projects
//! (recurring event series), tags and goals. It lives in the app data dir (never
//! in the notes folder) and is never reset, deleted or overwritten
//! automatically: schema changes are forward migrations keyed by
//! `PRAGMA user_version`, with a backup copy first. If it cannot be opened the
//! app still starts and these features report why.

use crate::google::{CalendarInfo, DEFAULT_CALENDAR_COLOR, SeriesInfo};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Backups of older schema versions that are kept next to the database.
const KEEP_BACKUPS: usize = 3;
const BUSY_TIMEOUT: Duration = Duration::from_millis(3000);

/// Migration `n` (0-based) takes the database from `user_version` n to n + 1.
const V1: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE projects(id INTEGER PRIMARY KEY,
                      google_calendar_id TEXT NOT NULL UNIQUE,
                      name TEXT NOT NULL,
                      color TEXT NOT NULL,
                      show_on_timeline INTEGER NOT NULL DEFAULT 1,
                      is_primary INTEGER NOT NULL DEFAULT 0,
                      archived INTEGER NOT NULL DEFAULT 0,
                      updated_at INTEGER NOT NULL);
CREATE TABLE tags(id INTEGER PRIMARY KEY AUTOINCREMENT,
                  name TEXT NOT NULL,
                  name_norm TEXT NOT NULL UNIQUE,
                  color TEXT NOT NULL DEFAULT 'sun',
                  created_at INTEGER NOT NULL);
CREATE TABLE goals(id INTEGER PRIMARY KEY AUTOINCREMENT,
                   project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
                   title TEXT,
                   minutes_per_day INTEGER NOT NULL CHECK (minutes_per_day BETWEEN 5 AND 720),
                   start TEXT NOT NULL,
                   days INTEGER CHECK (days IS NULL OR days >= 1),
                   archived INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL);
CREATE TABLE goal_tags(goal_id INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
                       tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
                       PRIMARY KEY (goal_id, tag_id));
";

/// v2 (M8c): the M8b `projects` (Google calendars) become `calendars`; `projects`
/// are now recurring event series (or one-off events the user chose). Goals that
/// pointed at a calendar lose that link; those left with no tag and no title are
/// archived (nothing is deleted). Dropping the old table does not touch `goals`:
/// the link is already NULL, and the foreign key then resolves to the new table.
const V2: &str = "
CREATE TABLE calendars(id INTEGER PRIMARY KEY,
                       google_calendar_id TEXT NOT NULL UNIQUE,
                       name TEXT NOT NULL,
                       color TEXT NOT NULL,
                       show_on_timeline INTEGER NOT NULL DEFAULT 1,
                       is_primary INTEGER NOT NULL DEFAULT 0,
                       archived INTEGER NOT NULL DEFAULT 0,
                       updated_at INTEGER NOT NULL);
INSERT INTO calendars(id, google_calendar_id, name, color, show_on_timeline, is_primary,
                      archived, updated_at)
  SELECT id, google_calendar_id, name, color, show_on_timeline, is_primary, archived, updated_at
  FROM projects;
UPDATE goals SET archived = 1, updated_at = CAST(strftime('%s', 'now') AS INTEGER)
  WHERE project_id IS NOT NULL
    AND (title IS NULL OR trim(title) = '')
    AND NOT EXISTS (SELECT 1 FROM goal_tags WHERE goal_tags.goal_id = goals.id);
UPDATE goals SET project_id = NULL WHERE project_id IS NOT NULL;
DROP TABLE projects;
CREATE TABLE projects(id INTEGER PRIMARY KEY AUTOINCREMENT,
                      kind TEXT NOT NULL CHECK (kind IN ('series', 'event')),
                      google_id TEXT NOT NULL,
                      calendar_id TEXT NOT NULL,
                      name TEXT NOT NULL,
                      archived INTEGER NOT NULL DEFAULT 0,
                      updated_at INTEGER NOT NULL,
                      UNIQUE (kind, google_id));
";

/// A v4 UUID made by SQLite itself (no crate): random bytes with the version and variant nibbles.
const SQL_UUID: &str = "lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || \
substr(hex(randomblob(2)), 2) || '-' || substr('89ab', abs(random()) % 4 + 1, 1) || \
substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6)))";

/// v3 (M9a): everything the account sync needs. Goals get a stable cross-device
/// `uid` (backfilled; a trigger fills it for any insert that doesn't set it), a
/// tombstone (`deleted_at`), the time of the last user change in ms (`changed_at`)
/// and a `dirty` flag (changed since the last sync). Existing rows start dirty
/// (goals, tags) so the first sync pushes them. Projects and calendars are
/// dirty only for what the user chose (archived / one-off event projects /
/// hidden calendars). Nothing is deleted or rewritten.
fn v3() -> String {
    format!(
        "
ALTER TABLE goals ADD COLUMN uid TEXT;
ALTER TABLE goals ADD COLUMN deleted_at INTEGER;
ALTER TABLE goals ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE goals ADD COLUMN dirty INTEGER NOT NULL DEFAULT 1;
ALTER TABLE goals ADD COLUMN pending_project_key TEXT;
UPDATE goals SET uid = {uuid}, changed_at = updated_at * 1000;
CREATE UNIQUE INDEX goals_uid ON goals(uid);
CREATE TRIGGER goals_uid_default AFTER INSERT ON goals WHEN NEW.uid IS NULL
BEGIN
  UPDATE goals SET uid = {uuid} WHERE id = NEW.id;
END;
ALTER TABLE tags ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tags ADD COLUMN dirty INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tags ADD COLUMN deleted_at INTEGER;
UPDATE tags SET changed_at = created_at * 1000;
ALTER TABLE projects ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0;
UPDATE projects SET changed_at = updated_at * 1000, dirty = 1 WHERE kind = 'event' OR archived = 1;
ALTER TABLE calendars ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE calendars ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0;
UPDATE calendars SET changed_at = updated_at * 1000, dirty = 1 WHERE show_on_timeline = 0;
",
        uuid = SQL_UUID
    )
}

/// A fresh UUID from SQLite, as the goal triggers/inserts make them.
pub(crate) fn new_uuid(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row(&format!("SELECT {SQL_UUID}"), [], |r| r.get(0))
}

pub fn system_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// v4 (M9b): what the notes-folder sync last saw per file on THIS computer (the rev it
/// last synced and the server's `seq` for it). Sync bookkeeping, not user data.
const V4: &str = "
CREATE TABLE file_sync (path TEXT PRIMARY KEY,
                        synced_rev TEXT,
                        remote_seq INTEGER);
";

fn migrations() -> Vec<String> {
    vec![V1.to_string(), V2.to_string(), v3(), V4.to_string()]
}

/// A Google calendar as the frontend sees it. It only decides what shows on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: i64,
    pub google_calendar_id: String,
    pub name: String,
    /// Google's `backgroundColor` (`#rrggbb`).
    pub color: String,
    pub show_on_timeline: bool,
    pub is_primary: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Series,
    Event,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Series => "series",
            ProjectKind::Event => "event",
        }
    }

    fn parse(s: &str) -> ProjectKind {
        if s == "event" {
            ProjectKind::Event
        } else {
            ProjectKind::Series
        }
    }
}

/// `series:<recurringEventId>` or `event:<eventId>`.
pub fn project_key(kind: ProjectKind, google_id: &str) -> String {
    format!("{}:{google_id}", kind.as_str())
}

/// A project (recurring event series, or a one-off event made a project) as the
/// frontend sees it. `color` and `calendar_name` come from its calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub kind: ProjectKind,
    pub key: String,
    pub name: String,
    pub color: String,
    pub calendar_name: String,
    pub archived: bool,
}

/// A project plus what the backend needs (backend only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub project: Project,
    pub google_id: String,
    pub calendar_id: String,
}

/// A series seen on a calendar: the calendar's Google id and the series.
pub type CalendarSeries = (String, Vec<SeriesInfo>);

pub struct AppDb {
    conn: Mutex<Connection>,
    /// Milliseconds clock for `changed_at` (injected in tests).
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

fn sql_err(context: &str, e: &rusqlite::Error) -> String {
    format!("{context}: {e}")
}

fn open_err(path: &Path, e: &rusqlite::Error) -> String {
    format!(
        "{} could not be opened ({e}); the file was left untouched",
        path.display()
    )
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `<db>.bak-v<version>` next to the database.
fn backup_path(db: &Path, version: i64) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(format!(".bak-v{version}"));
    PathBuf::from(name)
}

/// Copies the database (consistently, WAL included) before a migration.
fn make_backup(conn: &Connection, db: &Path, version: i64) -> Result<(), String> {
    let target = backup_path(db, version);
    // A backup of this version exists from an earlier, failed attempt: the
    // data it holds is the same, so keep it.
    if target.exists() {
        return Ok(());
    }
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
        .map_err(|e| format!("backing up the app database: {e}"))?;
    prune_backups(db);
    Ok(())
}

/// Keeps the newest `KEEP_BACKUPS` backups (by schema version).
fn prune_backups(db: &Path) {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let prefix = format!("{name}.bak-v");
    let mut found: Vec<(i64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let version = file.strip_prefix(&prefix)?.parse().ok()?;
            Some((version, e.path()))
        })
        .collect();
    found.sort_by_key(|(v, _)| std::cmp::Reverse(*v));
    for (_, path) in found.into_iter().skip(KEEP_BACKUPS) {
        let _ = fs::remove_file(path);
    }
}

/// Opens (creating if needed) and migrates. Every failure leaves the file as it was.
fn open_with<S: AsRef<str>>(path: &Path, migrations: &[S]) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let mut conn = Connection::open(path).map_err(|e| open_err(path, &e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| sql_err("busy timeout", &e))?;
    // The first real read: a damaged or foreign file fails here, before anything is written.
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| open_err(path, &e))?;
    let target = i64::try_from(migrations.len()).unwrap_or(i64::MAX);
    if version > target {
        return Err(format!(
            "{} was created by a newer version of the app (schema {version}, this app knows {target})",
            path.display()
        ));
    }
    if version == 0 {
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .map_err(|e| open_err(path, &e))?;
        if tables != 0 {
            return Err(format!(
                "{} is not a Catenga database; the file was left untouched",
                path.display()
            ));
        }
    }
    conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
        .map_err(|e| sql_err("journal_mode", &e))?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(|e| sql_err("synchronous", &e))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| sql_err("foreign_keys", &e))?;
    if version < target {
        if version > 0 {
            make_backup(&conn, path, version)?;
        }
        let tx = conn
            .transaction()
            .map_err(|e| sql_err("starting migration", &e))?;
        let first = usize::try_from(version).unwrap_or(0);
        for (i, sql) in migrations.iter().enumerate().skip(first) {
            tx.execute_batch(sql.as_ref())
                .map_err(|e| format!("migrating the app database to v{}: {e}", i + 1))?;
        }
        tx.pragma_update(None, "user_version", target)
            .map_err(|e| sql_err("setting user_version", &e))?;
        tx.commit()
            .map_err(|e| sql_err("committing migration", &e))?;
    }
    Ok(conn)
}

impl AppDb {
    pub fn open(path: &Path) -> Result<AppDb, String> {
        open_with(path, &migrations()).map(|conn| AppDb {
            conn: Mutex::new(conn),
            clock: Box::new(system_ms),
        })
    }

    #[cfg(test)]
    pub fn open_with_clock(
        path: &Path,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Result<AppDb, String> {
        open_with(path, &migrations()).map(|conn| AppDb {
            conn: Mutex::new(conn),
            clock: Box::new(clock),
        })
    }

    /// Now, in ms: the value user writes stamp into `changed_at`.
    pub fn ms(&self) -> i64 {
        (self.clock)()
    }

    /// Runs `f` on the connection under the lock; SQL errors become texts.
    pub fn with_conn<R>(
        &self,
        context: &str,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<R>,
    ) -> Result<R, String> {
        let mut conn = lock(&self.conn);
        f(&mut conn).map_err(|e| sql_err(context, &e))
    }

    // --------------------------------------------------------------- calendars

    /// Primary first, then live before archived, then by name.
    pub fn calendars(&self) -> Result<Vec<Calendar>, String> {
        self.with_conn("reading calendars", |conn| read_calendars(conn))
    }

    /// Mirrors Google's calendar list: upserts every listed calendar and
    /// archives the ones that are gone (never deleted).
    /// A calendar seen for the first time is shown when Google has it selected.
    pub fn upsert_calendars(&self, calendars: &[CalendarInfo], now: i64) -> Result<(), String> {
        self.with_conn("saving calendars", |conn| {
            let tx = conn.transaction()?;
            for c in calendars {
                tx.execute(
                    "INSERT INTO calendars(google_calendar_id, name, color, show_on_timeline,
                                           is_primary, archived, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
                     ON CONFLICT(google_calendar_id) DO UPDATE SET
                       name = excluded.name, color = excluded.color,
                       is_primary = excluded.is_primary, archived = 0,
                       updated_at = excluded.updated_at",
                    params![c.id, c.name, c.color, c.selected, c.primary, now],
                )?;
            }
            let mut stmt =
                tx.prepare("SELECT id, google_calendar_id FROM calendars WHERE archived = 0")?;
            let live: Vec<(i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for (id, calendar_id) in live {
                if !calendars.iter().any(|c| c.id == calendar_id) {
                    tx.execute(
                        "UPDATE calendars SET archived = 1, updated_at = ?2 WHERE id = ?1",
                        params![id, now],
                    )?;
                }
            }
            tx.commit()
        })
    }

    pub fn set_shown(&self, id: i64, shown: bool, now: i64) -> Result<(), String> {
        let ms = self.ms();
        let changed = self.with_conn("updating calendar", |conn| {
            conn.execute(
                "UPDATE calendars SET show_on_timeline = ?2, updated_at = ?3,
                                      changed_at = max(?4, changed_at + 1), dirty = 1
                 WHERE id = ?1",
                params![id, shown, now, ms],
            )
        })?;
        if changed == 0 {
            return Err("Calendar not found".into());
        }
        Ok(())
    }

    // ---------------------------------------------------------------- projects

    /// Live before archived, series before events, then by name.
    pub fn project_rows(&self) -> Result<Vec<ProjectRow>, String> {
        self.with_conn("reading projects", |conn| read_projects(conn))
    }

    pub fn projects(&self) -> Result<Vec<Project>, String> {
        Ok(self
            .project_rows()?
            .into_iter()
            .map(|r| r.project)
            .collect())
    }

    /// Records the series found on the calendars that were fetched successfully:
    /// upserts them (name = latest seen, calendar = where it lives; a project the
    /// user archived stays archived). Series of those calendars that were not returned
    /// (gone, or no longer repeating at least weekly) are removed — the owner doesn't
    /// want them listed — unless a goal points at one, which is archived instead.
    /// Sessions keep the series key, not the row, so nothing recorded is lost.
    /// Series of other calendars are left alone.
    /// Returns whether anything changed.
    pub fn sync_series(&self, found: &[CalendarSeries], now: i64) -> Result<bool, String> {
        self.with_conn("saving projects", |conn| {
            let tx = conn.transaction()?;
            let mut changed = false;
            for (calendar, series) in found {
                for s in series {
                    changed |= upsert_series(&tx, calendar, &s.id, &s.name, true, now)?;
                }
            }
            for (calendar, series) in found {
                let mut stmt = tx.prepare(
                    "SELECT p.id, p.google_id, p.archived,
                            EXISTS(SELECT 1 FROM goals g WHERE g.project_id = p.id)
                     FROM projects p WHERE p.kind = 'series' AND p.calendar_id = ?1",
                )?;
                let rows: Vec<(i64, String, bool, bool)> = stmt
                    .query_map([calendar], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                drop(stmt);
                for (id, google_id, archived, used) in rows {
                    if series.iter().any(|s| s.id == google_id) {
                        continue;
                    }
                    if used {
                        if !archived {
                            tx.execute(
                                "UPDATE projects SET archived = 1, updated_at = ?2 WHERE id = ?1",
                                params![id, now],
                            )?;
                            changed = true;
                        }
                    } else {
                        tx.execute("DELETE FROM projects WHERE id = ?1", [id])?;
                        changed = true;
                    }
                }
            }
            tx.commit()?;
            Ok(changed)
        })
    }

    /// Adds series that are not projects yet (name = the given title); existing projects are
    /// never touched. Returns whether a project was added. Test-only: in the app, projects come
    /// from `projects_refresh`, which knows how often a series repeats.
    #[cfg(test)]
    pub fn ensure_series(
        &self,
        seen: &[(String, String, String)],
        now: i64,
    ) -> Result<bool, String> {
        self.with_conn("saving projects", |conn| {
            let tx = conn.transaction()?;
            let mut added = false;
            for (calendar, recurring_id, name) in seen {
                let calendar = real_calendar_id(&tx, calendar)?;
                added |= upsert_series(&tx, &calendar, recurring_id, name, false, now)?;
            }
            tx.commit()?;
            Ok(added)
        })
    }

    /// Makes a one-off event a project (or brings it back if it was archived).
    pub fn project_from_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        name: &str,
        now: i64,
    ) -> Result<(), String> {
        let ms = self.ms();
        self.with_conn("saving project", |conn| {
            let tx = conn.transaction()?;
            let calendar = real_calendar_id(&tx, calendar_id)?;
            tx.execute(
                "INSERT INTO projects(kind, google_id, calendar_id, name, archived, updated_at,
                                      changed_at, dirty)
                 VALUES ('event', ?1, ?2, ?3, 0, ?4, ?5, 1)
                 ON CONFLICT(kind, google_id) DO UPDATE SET
                   calendar_id = excluded.calendar_id, name = excluded.name,
                   archived = 0, updated_at = excluded.updated_at,
                   changed_at = max(excluded.changed_at, changed_at + 1), dirty = 1",
                params![event_id, calendar, name, now, ms],
            )?;
            tx.commit()
        })
    }

    pub fn project_set_archived(&self, id: i64, archived: bool, now: i64) -> Result<(), String> {
        let ms = self.ms();
        let changed = self.with_conn("updating project", |conn| {
            conn.execute(
                "UPDATE projects SET archived = ?2, updated_at = ?3,
                                     changed_at = max(?4, changed_at + 1), dirty = 1
                 WHERE id = ?1",
                params![id, archived, now, ms],
            )
        })?;
        if changed == 0 {
            return Err("Project not found".into());
        }
        Ok(())
    }

    /// Meta flag helpers (one-time imports).
    pub fn flag(&self, key: &str) -> Result<bool, String> {
        self.with_conn("reading meta", |conn| flag_in(conn, key))
    }
}

/// The primary calendar's real id for the `primary` alias (when known).
fn real_calendar_id(conn: &Connection, calendar_id: &str) -> rusqlite::Result<String> {
    if calendar_id != "primary" {
        return Ok(calendar_id.to_string());
    }
    conn.query_row(
        "SELECT google_calendar_id FROM calendars WHERE is_primary = 1 AND archived = 0 LIMIT 1",
        [],
        |r| r.get(0),
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(calendar_id.to_string()),
        e => Err(e),
    })
}

/// Inserts a series project, or (when `refresh`) updates its name and calendar;
/// `archived` is never changed. Returns whether a row was added or changed.
fn upsert_series(
    tx: &Connection,
    calendar: &str,
    google_id: &str,
    name: &str,
    refresh: bool,
    now: i64,
) -> rusqlite::Result<bool> {
    let sql = if refresh {
        "INSERT INTO projects(kind, google_id, calendar_id, name, archived, updated_at)
         VALUES ('series', ?1, ?2, ?3, 0, ?4)
         ON CONFLICT(kind, google_id) DO UPDATE SET
           calendar_id = excluded.calendar_id, name = excluded.name,
           updated_at = excluded.updated_at
         WHERE calendar_id <> excluded.calendar_id OR name <> excluded.name"
    } else {
        "INSERT INTO projects(kind, google_id, calendar_id, name, archived, updated_at)
         VALUES ('series', ?1, ?2, ?3, 0, ?4)
         ON CONFLICT(kind, google_id) DO NOTHING"
    };
    Ok(tx.execute(sql, params![google_id, calendar, name, now])? > 0)
}

pub fn flag_in(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM meta WHERE key = ?1", [key], |_| Ok(()))
        .map(|()| true)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            e => Err(e),
        })
}

pub fn set_flag_in(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, '1')",
        [key],
    )
    .map(drop)
}

fn read_calendars(conn: &Connection) -> rusqlite::Result<Vec<Calendar>> {
    let mut stmt = conn.prepare(
        "SELECT id, google_calendar_id, name, color, show_on_timeline, is_primary, archived
         FROM calendars ORDER BY archived, is_primary DESC, name COLLATE NOCASE, id",
    )?;
    stmt.query_map([], |r| {
        Ok(Calendar {
            id: r.get(0)?,
            google_calendar_id: r.get(1)?,
            name: r.get(2)?,
            color: r.get(3)?,
            show_on_timeline: r.get(4)?,
            is_primary: r.get(5)?,
            archived: r.get(6)?,
        })
    })?
    .collect()
}

pub(crate) fn read_projects(conn: &Connection) -> rusqlite::Result<Vec<ProjectRow>> {
    // The calendar of a project: by id, or the primary one for the `primary` alias.
    let mut stmt = conn.prepare(
        "SELECT p.id, p.kind, p.google_id, p.calendar_id, p.name, p.archived,
                (SELECT c.color FROM calendars c
                  WHERE c.google_calendar_id = p.calendar_id
                     OR (p.calendar_id = 'primary' AND c.is_primary = 1) LIMIT 1),
                (SELECT c.name FROM calendars c
                  WHERE c.google_calendar_id = p.calendar_id
                     OR (p.calendar_id = 'primary' AND c.is_primary = 1) LIMIT 1)
         FROM projects p
         ORDER BY p.archived, p.kind = 'event', p.name COLLATE NOCASE, p.id",
    )?;
    stmt.query_map([], |r| {
        let kind = ProjectKind::parse(&r.get::<_, String>(1)?);
        let google_id: String = r.get(2)?;
        let calendar_id: String = r.get(3)?;
        let color: Option<String> = r.get(6)?;
        let calendar_name: Option<String> = r.get(7)?;
        Ok(ProjectRow {
            project: Project {
                id: r.get(0)?,
                kind,
                key: project_key(kind, &google_id),
                name: r.get(4)?,
                color: color.unwrap_or_else(|| DEFAULT_CALENDAR_COLOR.to_string()),
                calendar_name: calendar_name.unwrap_or_else(|| calendar_id.clone()),
                archived: r.get(5)?,
            },
            google_id,
            calendar_id,
        })
    })?
    .collect()
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use crate::config::test_util::TempDir;

    pub struct Fixture {
        pub tmp: TempDir,
        pub path: PathBuf,
        pub db: AppDb,
    }

    pub fn fixture() -> Fixture {
        let tmp = TempDir::new();
        let path = tmp.0.join("data").join("catenga.sqlite");
        let db = AppDb::open(&path).unwrap();
        Fixture { tmp, path, db }
    }

    pub fn cal(id: &str, name: &str, primary: bool, selected: bool) -> CalendarInfo {
        CalendarInfo {
            id: id.into(),
            name: name.into(),
            color: "#16a765".into(),
            primary,
            selected,
        }
    }

    pub fn series(id: &str, name: &str) -> SeriesInfo {
        SeriesInfo {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Adds the series `id` named `name` on `calendar` and returns its project id.
    pub fn add_series(db: &AppDb, calendar: &str, id: &str, name: &str) -> i64 {
        db.ensure_series(
            &[(calendar.to_string(), id.to_string(), name.to_string())],
            1,
        )
        .unwrap();
        db.project_rows()
            .unwrap()
            .into_iter()
            .find(|r| r.google_id == id)
            .unwrap()
            .project
            .id
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use crate::config::test_util::TempDir;

    fn user_version(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn fresh_database_gets_the_latest_schema_and_no_backup() {
        let f = fixture();
        assert_eq!(user_version(&f.path), 4);
        for table in [
            "meta",
            "calendars",
            "projects",
            "tags",
            "goals",
            "goal_tags",
        ] {
            let n: i64 =
                f.db.with_conn("t", |c| {
                    c.query_row(
                        "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                        [table],
                        |r| r.get(0),
                    )
                })
                .unwrap();
            assert_eq!(n, 1, "{table}");
        }
        let (fk, sync): (i64, i64) =
            f.db.with_conn("t", |c| {
                Ok((
                    c.pragma_query_value(None, "foreign_keys", |r| r.get(0))?,
                    c.pragma_query_value(None, "synchronous", |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!((fk, sync), (1, 2));
        let mode: String =
            f.db.with_conn("t", |c| {
                c.pragma_query_value(None, "journal_mode", |r| r.get(0))
            })
            .unwrap();
        assert_eq!(mode, "wal");
        assert!(
            names(f.path.parent().unwrap())
                .iter()
                .all(|n| !n.contains(".bak"))
        );
    }

    #[test]
    fn reopening_keeps_the_data_and_makes_no_backup() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("a@x", "Mestrado", true, true)], 1)
            .unwrap();
        add_series(&f.db, "a@x", "s1", "Learn Rust");
        drop(f.db);
        let db = AppDb::open(&f.path).unwrap();
        assert_eq!(db.calendars().unwrap().len(), 1);
        assert_eq!(db.projects().unwrap().len(), 1);
        assert!(
            names(f.path.parent().unwrap())
                .iter()
                .all(|n| !n.contains(".bak"))
        );
    }

    #[test]
    fn migrating_an_existing_file_backs_it_up_first() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        drop(open_with(&path, &[V1]).unwrap());
        {
            let c = Connection::open(&path).unwrap();
            c.execute(
                "INSERT INTO tags(name, name_norm, color, created_at) VALUES ('rust','rust','sun',1)",
                [],
            )
            .unwrap();
        }
        let conn = open_with(&path, &[V1, "ALTER TABLE tags ADD COLUMN note TEXT;"]).unwrap();
        assert_eq!(user_version(&path), 2);
        // The new column exists and the row survived.
        let n: i64 = conn
            .query_row("SELECT count(*) FROM tags WHERE note IS NULL", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
        // The backup is the v1 file with the data and without the new column.
        let bak = backup_path(&path, 1);
        assert!(bak.exists());
        assert_eq!(user_version(&bak), 1);
        let b = Connection::open(&bak).unwrap();
        let n: i64 = b
            .query_row("SELECT count(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert!(b.prepare("SELECT note FROM tags").is_err());
    }

    /// A real v1 file: two calendars, tags, and goals of every shape.
    fn build_v1(path: &Path) {
        let c = open_with(path, &[V1]).unwrap();
        c.execute_batch(
            "INSERT INTO projects(id, google_calendar_id, name, color, show_on_timeline,
                                  is_primary, archived, updated_at)
               VALUES (1, 'me@x', 'Me', '#111111', 1, 1, 0, 5),
                      (2, 'mest@x', 'Mestrado', '#222222', 0, 0, 1, 6);
             INSERT INTO tags(id, name, name_norm, color, created_at) VALUES (1, 'rust', 'rust', 'leaf', 1);
             -- 1: calendar only; 2: calendar + tag; 3: calendar + title; 4: tag only; 5: title only
             INSERT INTO goals(id, project_id, title, minutes_per_day, start, days, archived, created_at, updated_at) VALUES
               (1, 2, NULL, 60, '2026-10-01', NULL, 0, 1, 1),
               (2, 2, NULL, 30, '2026-10-02', 5, 0, 1, 1),
               (3, 1, 'Deep work', 45, '2026-10-03', NULL, 0, 1, 1),
               (4, NULL, NULL, 20, '2026-10-04', NULL, 0, 1, 1),
               (5, NULL, 'Rust', 25, '2026-10-05', 3, 1, 1, 1);
             INSERT INTO goal_tags(goal_id, tag_id) VALUES (2, 1), (4, 1);",
        )
        .unwrap();
    }

    #[test]
    fn v1_to_v2_keeps_every_row_repoints_goals_and_backs_up() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v1(&path);
        let db = AppDb::open(&path).unwrap();
        assert_eq!(user_version(&path), 4);

        // The old projects are the calendars, ids and choices intact.
        let cals = db.calendars().unwrap();
        let got: Vec<_> = cals
            .iter()
            .map(|c| {
                (
                    c.id,
                    c.google_calendar_id.as_str(),
                    c.name.as_str(),
                    c.color.as_str(),
                    c.show_on_timeline,
                    c.is_primary,
                    c.archived,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (1, "me@x", "Me", "#111111", true, true, false),
                (2, "mest@x", "Mestrado", "#222222", false, false, true),
            ]
        );
        // The new projects table starts empty.
        assert!(db.projects().unwrap().is_empty());

        // Goals: all five survive; none points at a project; the ones that only had a
        // calendar are archived; tags stay.
        type GoalRow = (i64, Option<i64>, Option<String>, bool, i64);
        let rows: Vec<GoalRow> = db
            .with_conn("t", |c| {
                c.prepare(
                    "SELECT g.id, g.project_id, g.title, g.archived,
                            (SELECT count(*) FROM goal_tags t WHERE t.goal_id = g.id)
                     FROM goals g ORDER BY g.id",
                )?
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect()
            })
            .unwrap();
        assert_eq!(
            rows,
            [
                (1, None, None, true, 0),
                (2, None, None, false, 1),
                (3, None, Some("Deep work".into()), false, 0),
                (4, None, None, false, 1),
                (5, None, Some("Rust".into()), true, 0),
            ]
        );
        assert_eq!(
            db.with_conn("t", |c| c
                .query_row("SELECT count(*) FROM tags", [], |r| r.get::<_, i64>(0)))
                .unwrap(),
            1
        );

        // The backup is the untouched v1 file.
        let bak = backup_path(&path, 1);
        assert_eq!(user_version(&bak), 1);
        let b = Connection::open(&bak).unwrap();
        let n: i64 = b
            .query_row(
                "SELECT count(*) FROM goals WHERE project_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3, "the backup still has the old links");

        // Goals can point at the new projects (the foreign key resolves to them), and
        // dropping a project still unlinks instead of deleting the goal.
        let pid = add_series(&db, "me@x", "ser", "Learn Rust");
        db.with_conn("t", |c| {
            c.execute("UPDATE goals SET project_id = ?1 WHERE id = 3", [pid])
        })
        .unwrap();
        let bad = db.with_conn("t", |c| {
            c.execute("UPDATE goals SET project_id = 999 WHERE id = 3", [])
        });
        assert!(bad.is_err(), "foreign key enforced against the new table");
        db.with_conn("t", |c| c.execute("DELETE FROM projects", []))
            .unwrap();
        assert_eq!(
            db.with_conn("t", |c| c.query_row(
                "SELECT count(*) FROM goals",
                [],
                |r| r.get::<_, i64>(0)
            ))
            .unwrap(),
            5
        );
    }

    #[test]
    fn v1_to_v2_is_idempotent_on_reopen() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v1(&path);
        drop(AppDb::open(&path).unwrap());
        let db = AppDb::open(&path).unwrap();
        assert_eq!(db.calendars().unwrap().len(), 2);
        assert_eq!(user_version(&path), 4);
    }

    /// A real v2 file with data of every kind the v3 migration touches.
    fn build_v2(path: &Path) {
        let c = open_with(path, &[V1, V2]).unwrap();
        c.execute_batch(
            "INSERT INTO calendars(id, google_calendar_id, name, color, show_on_timeline, is_primary, archived, updated_at)
               VALUES (1, 'me@x', 'Me', '#111111', 1, 1, 0, 5), (2, 'hid@x', 'Hidden', '#222222', 0, 0, 0, 6);
             INSERT INTO projects(id, kind, google_id, calendar_id, name, archived, updated_at)
               VALUES (1, 'series', 's1', 'me@x', 'Learn Rust', 0, 7),
                      (2, 'series', 's2', 'me@x', 'Gym', 1, 8),
                      (3, 'event', 'e1', 'me@x', 'Dentist', 0, 9);
             INSERT INTO tags(id, name, name_norm, color, created_at) VALUES (1, 'Rust', 'rust', 'leaf', 3);
             INSERT INTO goals(id, project_id, title, minutes_per_day, start, days, archived, created_at, updated_at) VALUES
               (1, 1, NULL, 60, '2026-10-01', NULL, 0, 1, 11),
               (2, NULL, 'Deep', 30, '2026-10-02', 5, 1, 1, 12);
             INSERT INTO goal_tags(goal_id, tag_id) VALUES (1, 1);",
        )
        .unwrap();
    }

    #[test]
    fn v2_to_v3_keeps_every_row_adds_uids_and_marks_user_data_for_sync() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v2(&path);
        let db = AppDb::open(&path).unwrap();
        assert_eq!(user_version(&path), 4);
        // Backup of v2, untouched, without the new columns.
        let bak = backup_path(&path, 2);
        assert_eq!(user_version(&bak), 2);
        assert!(
            Connection::open(&bak)
                .unwrap()
                .prepare("SELECT uid FROM goals")
                .is_err()
        );
        // Nothing lost.
        let count = |t: &str| -> i64 {
            db.with_conn("t", |c| {
                c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            })
            .unwrap()
        };
        assert_eq!(
            [
                count("calendars"),
                count("projects"),
                count("tags"),
                count("goals"),
                count("goal_tags")
            ],
            [2, 3, 1, 2, 1]
        );
        // Goals: distinct v4 uids, changed_at from updated_at (ms), dirty, linked rows intact.
        type G = (i64, String, i64, bool, Option<i64>);
        let goals: Vec<G> = db
            .with_conn("t", |c| {
                c.prepare("SELECT id, uid, changed_at, dirty, project_id FROM goals ORDER BY id")?
                    .query_map([], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .collect()
            })
            .unwrap();
        assert_eq!((goals[0].2, goals[1].2), (11_000, 12_000));
        assert!(
            goals
                .iter()
                .all(|g| g.3 && g.1.len() == 36 && g.1.as_bytes()[14] == b'4')
        );
        assert_ne!(goals[0].1, goals[1].1);
        assert_eq!(goals[0].4, Some(1));
        assert_eq!(db.goal_views(false).unwrap().len(), 1);
        assert_eq!(db.goal_views(true).unwrap().len(), 1);
        // Tags are dirty with their creation time; only user choices are dirty on projects/calendars.
        let dirty = |t: &str| -> Vec<i64> {
            db.with_conn("t", |c| {
                c.prepare(&format!("SELECT id FROM {t} WHERE dirty = 1 ORDER BY id"))?
                    .query_map([], |r| r.get(0))?
                    .collect()
            })
            .unwrap()
        };
        assert_eq!(dirty("tags"), [1]);
        assert_eq!(dirty("projects"), [2, 3], "archived and one-off event");
        assert_eq!(dirty("calendars"), [2], "the hidden one");
        // New goals made later get their own uid.
        db.goal_create(
            &crate::goals::GoalDraft {
                project_id: None,
                tag_names: vec![],
                title: Some("New".into()),
                minutes_per_day: 20,
                start: "2026-10-03".into(),
                days: None,
            },
            1,
        )
        .unwrap();
        assert_eq!(count("goals"), 3);
        let distinct: i64 = db
            .with_conn("t", |c| {
                c.query_row("SELECT count(DISTINCT uid) FROM goals", [], |r| r.get(0))
            })
            .unwrap();
        assert_eq!(distinct, 3);
        drop(db);
        // Reopening is a no-op (no second backup run, same data).
        let db = AppDb::open(&path).unwrap();
        assert_eq!(count_goals(&db), 3);
    }

    fn count_goals(db: &AppDb) -> i64 {
        db.with_conn("t", |c| {
            c.query_row("SELECT count(*) FROM goals", [], |r| r.get(0))
        })
        .unwrap()
    }

    #[test]
    fn a_failing_v3_rolls_back_to_the_untouched_v2() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v2(&path);
        let err = open_with(
            &path,
            &[V1.to_string(), V2.to_string(), format!("{} NOT SQL;", v3())],
        )
        .unwrap_err();
        assert!(err.contains("to v3"), "{err}");
        assert_eq!(user_version(&path), 2);
        let c = Connection::open(&path).unwrap();
        assert!(c.prepare("SELECT uid FROM goals").is_err());
        assert_eq!(
            c.query_row("SELECT count(*) FROM goals", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    /// A real v3 file with user data and an M9a sync cursor.
    fn build_v3(path: &Path) {
        let c = open_with(path, &[V1.to_string(), V2.to_string(), v3()]).unwrap();
        c.execute_batch(
            "INSERT INTO tags(name, name_norm, color, created_at) VALUES ('Rust', 'rust', 'leaf', 3);
             INSERT INTO goals(minutes_per_day, start, created_at, updated_at) VALUES (60, '2026-10-01', 1, 2);
             INSERT INTO meta(key, value) VALUES ('sync_cursor:u1', '42');",
        )
        .unwrap();
    }

    #[test]
    fn v3_to_v4_adds_the_file_sync_table_keeps_every_row_and_backs_up() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v3(&path);
        let db = AppDb::open(&path).unwrap();
        assert_eq!(user_version(&path), 4);
        let cols: Vec<String> = db
            .with_conn("t", |c| {
                c.prepare("SELECT name FROM pragma_table_info('file_sync') ORDER BY cid")?
                    .query_map([], |r| r.get(0))?
                    .collect()
            })
            .unwrap();
        assert_eq!(cols, ["path", "synced_rev", "remote_seq"]);
        let n = |sql: &str| -> i64 {
            db.with_conn("t", |c| c.query_row(sql, [], |r| r.get(0)))
                .unwrap()
        };
        assert_eq!(n("SELECT count(*) FROM file_sync"), 0);
        assert_eq!(n("SELECT count(*) FROM tags"), 1);
        assert_eq!(n("SELECT count(*) FROM goals"), 1);
        assert_eq!(
            n("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'sync_cursor:u1'"),
            42
        );
        // The backup is the untouched v3 file.
        let bak = backup_path(&path, 3);
        assert!(bak.exists());
        assert_eq!(user_version(&bak), 3);
        let b = Connection::open(&bak).unwrap();
        assert!(b.prepare("SELECT path FROM file_sync").is_err());
        assert_eq!(
            b.query_row("SELECT count(*) FROM goals", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // Reopening is a no-op.
        drop(db);
        assert_eq!(n_goals(&path), 1);
    }

    fn n_goals(path: &Path) -> i64 {
        AppDb::open(path)
            .unwrap()
            .with_conn("t", |c| {
                c.query_row("SELECT count(*) FROM goals", [], |r| r.get(0))
            })
            .unwrap()
    }

    #[test]
    fn a_failing_v4_rolls_back_to_the_untouched_v3() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        build_v3(&path);
        let err = open_with(
            &path,
            &[
                V1.to_string(),
                V2.to_string(),
                v3(),
                format!("{V4} NOT SQL;"),
            ],
        )
        .unwrap_err();
        assert!(err.contains("to v4"), "{err}");
        assert_eq!(user_version(&path), 3);
        let c = Connection::open(&path).unwrap();
        assert!(c.prepare("SELECT path FROM file_sync").is_err());
        assert_eq!(
            c.query_row("SELECT count(*) FROM tags", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn only_the_last_three_backups_are_kept() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        let steps = [
            V1,
            "ALTER TABLE tags ADD COLUMN a TEXT;",
            "ALTER TABLE tags ADD COLUMN b TEXT;",
            "ALTER TABLE tags ADD COLUMN c TEXT;",
            "ALTER TABLE tags ADD COLUMN d TEXT;",
        ];
        for n in 1..=5 {
            drop(open_with(&path, &steps[..n]).unwrap());
        }
        let baks: Vec<_> = names(&tmp.0)
            .into_iter()
            .filter(|n| n.contains(".bak-v"))
            .collect();
        assert_eq!(
            baks,
            [
                "catenga.sqlite.bak-v2",
                "catenga.sqlite.bak-v3",
                "catenga.sqlite.bak-v4"
            ]
        );
    }

    #[test]
    fn a_failing_migration_rolls_back_and_keeps_the_data() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        drop(open_with(&path, &[V1]).unwrap());
        let err = open_with(
            &path,
            &[V1, "ALTER TABLE tags ADD COLUMN ok TEXT; THIS IS NOT SQL;"],
        )
        .unwrap_err();
        assert!(err.contains("migrating the app database to v2"), "{err}");
        assert_eq!(user_version(&path), 1);
        let c = Connection::open(&path).unwrap();
        assert!(c.prepare("SELECT ok FROM tags").is_err());
        assert!(backup_path(&path, 1).exists());
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_stays_byte_for_byte() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        let junk = b"this is definitely not a sqlite database, it is just text padding".repeat(40);
        fs::write(&path, &junk).unwrap();
        let err = AppDb::open(&path).err().expect("must fail");
        assert!(err.contains("could not be opened"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), junk);
        assert_eq!(names(&tmp.0), ["catenga.sqlite"]);
    }

    #[test]
    fn a_database_from_a_newer_app_is_refused_untouched() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        drop(
            open_with(
                &path,
                &[
                    V1.to_string(),
                    V2.to_string(),
                    v3(),
                    V4.to_string(),
                    "SELECT 1;".to_string(),
                ],
            )
            .unwrap(),
        );
        let before = fs::read(&path).unwrap();
        let err = AppDb::open(&path).err().expect("must fail");
        assert!(err.contains("newer version"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(user_version(&path), 5);
    }

    #[test]
    fn a_foreign_sqlite_file_is_refused_untouched() {
        let tmp = TempDir::new();
        let path = tmp.0.join("catenga.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE other(x); INSERT INTO other VALUES (1);")
                .unwrap();
        }
        let before = fs::read(&path).unwrap();
        let err = AppDb::open(&path).err().expect("must fail");
        assert!(err.contains("not a Catenga database"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn calendars_upsert_keep_the_users_choice_and_archive_the_missing() {
        let f = fixture();
        f.db.upsert_calendars(
            &[
                cal("me@x", "me@x", true, true),
                cal("mest@x", "Mestrado", false, true),
                cal("holi@x", "Holidays", false, false),
            ],
            1,
        )
        .unwrap();
        let rows = f.db.calendars().unwrap();
        let by = |cid: &str| {
            rows.iter()
                .find(|r| r.google_calendar_id == cid)
                .unwrap()
                .clone()
        };
        assert!(by("mest@x").show_on_timeline);
        assert!(!by("holi@x").show_on_timeline, "unselected in Google");
        assert!(by("me@x").is_primary);
        // The user hides Mestrado; Google renames it and drops Holidays.
        f.db.set_shown(by("mest@x").id, false, 2).unwrap();
        let mut renamed = cal("mest@x", "Mestrado 2026", false, true);
        renamed.color = "#ff0000".into();
        f.db.upsert_calendars(&[cal("me@x", "Me", true, true), renamed], 3)
            .unwrap();
        let rows = f.db.calendars().unwrap();
        let by = |cid: &str| {
            rows.iter()
                .find(|r| r.google_calendar_id == cid)
                .unwrap()
                .clone()
        };
        let m = by("mest@x");
        assert_eq!(
            (m.name.as_str(), m.color.as_str()),
            ("Mestrado 2026", "#ff0000")
        );
        assert!(
            !m.show_on_timeline,
            "choice kept although Google says selected"
        );
        assert!(by("holi@x").archived);
        assert!(!by("me@x").archived);
        assert_eq!(by("me@x").name, "Me");
        // It comes back: un-archived, still the same row.
        let id = by("holi@x").id;
        f.db.upsert_calendars(&[cal("holi@x", "Holidays", false, false)], 4)
            .unwrap();
        let back = f.db.calendars().unwrap();
        assert!(!back.iter().find(|c| c.id == id).unwrap().archived);
        assert_eq!(back.len(), 3, "never deleted");
    }

    #[test]
    fn calendars_are_ordered_primary_first_archived_last() {
        let f = fixture();
        f.db.upsert_calendars(
            &[
                cal("z@x", "Zeta", false, true),
                cal("a@x", "alpha", false, true),
                cal("me@x", "Me", true, true),
                cal("old@x", "Old", false, true),
            ],
            1,
        )
        .unwrap();
        f.db.upsert_calendars(
            &[
                cal("z@x", "Zeta", false, true),
                cal("a@x", "alpha", false, true),
                cal("me@x", "Me", true, true),
            ],
            2,
        )
        .unwrap();
        let order: Vec<_> =
            f.db.calendars()
                .unwrap()
                .into_iter()
                .map(|p| p.name)
                .collect();
        assert_eq!(order, ["Me", "alpha", "Zeta", "Old"]);
    }

    #[test]
    fn set_shown_on_an_unknown_calendar_errors() {
        let f = fixture();
        assert_eq!(
            f.db.set_shown(99, true, 1).unwrap_err(),
            "Calendar not found"
        );
    }

    #[test]
    fn calendar_json_shape() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "Me", true, true)], 1)
            .unwrap();
        let p = f.db.calendars().unwrap();
        assert_eq!(
            serde_json::to_value(&p[0]).unwrap(),
            serde_json::json!({"id":1,"googleCalendarId":"me@x","name":"Me","color":"#16a765","showOnTimeline":true,"isPrimary":true,"archived":false})
        );
    }

    #[test]
    fn project_json_shape_has_key_and_the_calendars_color_and_name() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "Personal", true, true)], 1)
            .unwrap();
        add_series(&f.db, "me@x", "abc123", "Learn Rust");
        f.db.project_from_event("me@x", "ev9", "Dentist", 2)
            .unwrap();
        let p = f.db.projects().unwrap();
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!([
                {"id":1,"kind":"series","key":"series:abc123","name":"Learn Rust","color":"#16a765","calendarName":"Personal","archived":false},
                {"id":2,"kind":"event","key":"event:ev9","name":"Dentist","color":"#16a765","calendarName":"Personal","archived":false}
            ])
        );
    }

    #[test]
    fn a_project_without_a_known_calendar_gets_the_default_color_and_the_calendar_id() {
        let f = fixture();
        add_series(&f.db, "ghost@x", "s", "Orphan");
        let p = &f.db.projects().unwrap()[0];
        assert_eq!(p.color, DEFAULT_CALENDAR_COLOR);
        assert_eq!(p.calendar_name, "ghost@x");
    }

    #[test]
    fn sync_series_upserts_renames_and_drops_missing_ones_of_fetched_calendars_only() {
        let f = fixture();
        let db = &f.db;
        db.sync_series(
            &[
                (
                    "a@x".into(),
                    vec![series("s1", "Learn Rust"), series("s2", "Gym")],
                ),
                ("b@x".into(), vec![series("s3", "Work")]),
            ],
            1,
        )
        .unwrap();
        assert_eq!(db.projects().unwrap().len(), 3);
        // a@x: Learn Rust renamed, Gym gone. b@x was not fetched this time (failed).
        let changed = db
            .sync_series(&[("a@x".into(), vec![series("s1", "Rust 2")])], 2)
            .unwrap();
        assert!(changed);
        let by_name = |db: &AppDb| {
            db.projects()
                .unwrap()
                .into_iter()
                .map(|p| (p.name, p.archived))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            by_name(db),
            // Gym is gone from the list entirely (owner: don't show what isn't a project).
            [("Rust 2".to_string(), false), ("Work".to_string(), false)]
        );
        // Unchanged run reports nothing changed.
        assert!(
            !db.sync_series(&[("a@x".into(), vec![series("s1", "Rust 2")])], 3)
                .unwrap()
        );
    }

    #[test]
    fn a_missing_series_used_by_a_goal_is_archived_not_removed() {
        let f = fixture();
        let used = add_series(&f.db, "a@x", "s1", "Learn Rust");
        add_series(&f.db, "a@x", "s2", "Rent (monthly)");
        f.db.with_conn("test", |c| {
            c.execute(
                "INSERT INTO goals(project_id, minutes_per_day, start, days, archived, created_at, updated_at)
                 VALUES (?1, 60, '2026-10-21', NULL, 0, 1, 1)",
                [used],
            )
            .map(|_| ())
        })
        .unwrap();
        f.db.sync_series(&[("a@x".into(), vec![])], 2).unwrap();
        let left: Vec<_> =
            f.db.projects()
                .unwrap()
                .into_iter()
                .map(|p| (p.name, p.archived))
                .collect();
        assert_eq!(left, [("Learn Rust".to_string(), true)]);
    }

    #[test]
    fn an_archived_series_that_disappears_is_removed_too() {
        let f = fixture();
        let id = add_series(&f.db, "a@x", "s1", "Birthday (yearly)");
        f.db.project_set_archived(id, true, 2).unwrap();
        f.db.sync_series(&[("a@x".into(), vec![])], 3).unwrap();
        assert!(f.db.projects().unwrap().is_empty());
    }

    #[test]
    fn a_user_archived_project_stays_archived_when_google_still_returns_it() {
        let f = fixture();
        let id = add_series(&f.db, "a@x", "s1", "Learn Rust");
        f.db.project_set_archived(id, true, 2).unwrap();
        f.db.sync_series(&[("a@x".into(), vec![series("s1", "Learn Rust")])], 3)
            .unwrap();
        assert!(f.db.projects().unwrap()[0].archived);
        f.db.project_set_archived(id, false, 4).unwrap();
        assert!(!f.db.projects().unwrap()[0].archived);
        assert_eq!(
            f.db.project_set_archived(99, true, 1).unwrap_err(),
            "Project not found"
        );
    }

    #[test]
    fn a_series_that_moved_calendars_follows_and_is_not_archived() {
        let f = fixture();
        add_series(&f.db, "a@x", "s1", "Learn Rust");
        f.db.sync_series(
            &[
                ("a@x".into(), vec![]),
                ("b@x".into(), vec![series("s1", "Learn Rust")]),
            ],
            2,
        )
        .unwrap();
        let rows = f.db.project_rows().unwrap();
        assert_eq!(rows[0].calendar_id, "b@x");
        assert!(!rows[0].project.archived);
    }

    #[test]
    fn ensure_series_adds_missing_ones_and_never_touches_existing_ones() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "Me", true, true)], 1)
            .unwrap();
        let id = add_series(&f.db, "me@x", "s1", "Learn Rust");
        f.db.project_set_archived(id, true, 2).unwrap();
        let added =
            f.db.ensure_series(
                &[
                    ("me@x".into(), "s1".into(), "Renamed instance".into()),
                    ("primary".into(), "s2".into(), "Gym".into()),
                ],
                3,
            )
            .unwrap();
        assert!(added);
        let rows = f.db.project_rows().unwrap();
        let s1 = rows.iter().find(|r| r.google_id == "s1").unwrap();
        assert_eq!(
            (s1.project.name.as_str(), s1.project.archived),
            ("Learn Rust", true)
        );
        let s2 = rows.iter().find(|r| r.google_id == "s2").unwrap();
        assert_eq!(
            s2.calendar_id, "me@x",
            "the primary alias becomes the real id"
        );
        assert!(
            !f.db
                .ensure_series(&[("me@x".into(), "s2".into(), "Gym".into())], 4)
                .unwrap()
        );
    }

    #[test]
    fn project_from_event_is_idempotent_and_restores() {
        let f = fixture();
        f.db.project_from_event("me@x", "ev", "Dentist", 1).unwrap();
        let id = f.db.projects().unwrap()[0].id;
        f.db.project_set_archived(id, true, 2).unwrap();
        f.db.project_from_event("me@x", "ev", "Dentist visit", 3)
            .unwrap();
        let p = f.db.projects().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            (p[0].name.as_str(), p[0].archived),
            ("Dentist visit", false)
        );
    }

    #[test]
    fn projects_list_live_first_series_before_events_then_by_name() {
        let f = fixture();
        add_series(&f.db, "a@x", "s1", "zeta");
        add_series(&f.db, "a@x", "s2", "Alpha");
        let old = add_series(&f.db, "a@x", "s3", "Old");
        f.db.project_from_event("a@x", "e1", "AAA event", 1)
            .unwrap();
        f.db.project_set_archived(old, true, 2).unwrap();
        let order: Vec<_> =
            f.db.projects()
                .unwrap()
                .into_iter()
                .map(|p| p.name)
                .collect();
        assert_eq!(order, ["Alpha", "zeta", "AAA event", "Old"]);
    }

    #[test]
    fn meta_flags() {
        let f = fixture();
        assert!(!f.db.flag("x").unwrap());
        f.db.with_conn("t", |c| set_flag_in(c, "x")).unwrap();
        assert!(f.db.flag("x").unwrap());
    }
}
