//! `catenga.sqlite`: the app database, a SOURCE OF TRUTH for projects, tags and
//! goals. It lives in the app data dir (never in the notes folder) and is never
//! reset, deleted or overwritten automatically: schema changes are forward
//! migrations keyed by `PRAGMA user_version`, with a backup copy first. If it
//! cannot be opened the app still starts and these features report why.

use crate::google::CalendarInfo;
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

const MIGRATIONS: &[&str] = &[V1];

/// A Google calendar as the frontend sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub google_calendar_id: String,
    pub name: String,
    /// Google's `backgroundColor` (`#rrggbb`).
    pub color: String,
    pub show_on_timeline: bool,
    pub is_primary: bool,
    pub archived: bool,
}

/// A project plus the Google calendar it stands for (backend only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub project: Project,
    pub calendar_id: String,
}

pub struct AppDb {
    conn: Mutex<Connection>,
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
fn open_with(path: &Path, migrations: &[&str]) -> Result<Connection, String> {
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
            tx.execute_batch(sql)
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
        open_with(path, MIGRATIONS).map(|conn| AppDb {
            conn: Mutex::new(conn),
        })
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

    // ---------------------------------------------------------------- projects

    /// Primary first, then live before archived, then by name.
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

    pub fn project(&self, id: i64) -> Result<ProjectRow, String> {
        self.project_rows()?
            .into_iter()
            .find(|r| r.project.id == id)
            .ok_or_else(|| "Project not found".to_string())
    }

    /// Mirrors Google's calendar list: upserts every listed calendar and
    /// archives the ones that are gone (never deleted: goals point at them).
    /// A calendar seen for the first time is shown when Google has it selected.
    pub fn upsert_calendars(&self, calendars: &[CalendarInfo], now: i64) -> Result<(), String> {
        self.with_conn("saving projects", |conn| {
            let tx = conn.transaction()?;
            for c in calendars {
                tx.execute(
                    "INSERT INTO projects(google_calendar_id, name, color, show_on_timeline,
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
                tx.prepare("SELECT id, google_calendar_id FROM projects WHERE archived = 0")?;
            let live: Vec<(i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for (id, calendar_id) in live {
                if !calendars.iter().any(|c| c.id == calendar_id) {
                    tx.execute(
                        "UPDATE projects SET archived = 1, updated_at = ?2 WHERE id = ?1",
                        params![id, now],
                    )?;
                }
            }
            tx.commit()
        })
    }

    pub fn set_shown(&self, id: i64, shown: bool, now: i64) -> Result<(), String> {
        let changed = self.with_conn("updating project", |conn| {
            conn.execute(
                "UPDATE projects SET show_on_timeline = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, shown, now],
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

pub(crate) fn read_projects(conn: &Connection) -> rusqlite::Result<Vec<ProjectRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, google_calendar_id, name, color, show_on_timeline, is_primary, archived
         FROM projects ORDER BY archived, is_primary DESC, name COLLATE NOCASE, id",
    )?;
    stmt.query_map([], |r| {
        Ok(ProjectRow {
            calendar_id: r.get(1)?,
            project: Project {
                id: r.get(0)?,
                google_calendar_id: r.get(1)?,
                name: r.get(2)?,
                color: r.get(3)?,
                show_on_timeline: r.get(4)?,
                is_primary: r.get(5)?,
                archived: r.get(6)?,
            },
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
    fn fresh_database_gets_schema_v1_and_no_backup() {
        let f = fixture();
        assert_eq!(user_version(&f.path), 1);
        for table in ["meta", "projects", "tags", "goals", "goal_tags"] {
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
        drop(f.db);
        let db = AppDb::open(&f.path).unwrap();
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
        drop(open_with(&path, &[V1, "SELECT 1;"]).unwrap());
        let before = fs::read(&path).unwrap();
        let err = AppDb::open(&path).err().expect("must fail");
        assert!(err.contains("newer version"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(user_version(&path), 2);
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
        let rows = f.db.project_rows().unwrap();
        let by = |cid: &str| rows.iter().find(|r| r.calendar_id == cid).unwrap().clone();
        assert!(by("mest@x").project.show_on_timeline);
        assert!(
            !by("holi@x").project.show_on_timeline,
            "unselected in Google"
        );
        assert!(by("me@x").project.is_primary);
        // The user hides Mestrado; Google renames it and drops Holidays.
        f.db.set_shown(by("mest@x").project.id, false, 2).unwrap();
        let mut renamed = cal("mest@x", "Mestrado 2026", false, true);
        renamed.color = "#ff0000".into();
        f.db.upsert_calendars(&[cal("me@x", "Me", true, true), renamed], 3)
            .unwrap();
        let rows = f.db.project_rows().unwrap();
        let by = |cid: &str| rows.iter().find(|r| r.calendar_id == cid).unwrap().clone();
        let m = by("mest@x").project;
        assert_eq!(
            (m.name.as_str(), m.color.as_str()),
            ("Mestrado 2026", "#ff0000")
        );
        assert!(
            !m.show_on_timeline,
            "choice kept although Google says selected"
        );
        assert!(by("holi@x").project.archived);
        assert!(!by("me@x").project.archived);
        assert_eq!(by("me@x").project.name, "Me");
        // It comes back: un-archived, still the same row.
        let id = by("holi@x").project.id;
        f.db.upsert_calendars(&[cal("holi@x", "Holidays", false, false)], 4)
            .unwrap();
        assert!(!f.db.project(id).unwrap().project.archived);
        assert_eq!(f.db.project_rows().unwrap().len(), 3, "never deleted");
    }

    #[test]
    fn projects_are_ordered_primary_first_archived_last() {
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
            f.db.projects()
                .unwrap()
                .into_iter()
                .map(|p| p.name)
                .collect();
        assert_eq!(order, ["Me", "alpha", "Zeta", "Old"]);
    }

    #[test]
    fn set_shown_on_an_unknown_project_errors() {
        let f = fixture();
        assert_eq!(
            f.db.set_shown(99, true, 1).unwrap_err(),
            "Project not found"
        );
        assert_eq!(f.db.project(99).unwrap_err(), "Project not found");
    }

    #[test]
    fn project_json_shape() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "Me", true, true)], 1)
            .unwrap();
        let p = f.db.projects().unwrap();
        assert_eq!(
            serde_json::to_value(&p[0]).unwrap(),
            serde_json::json!({"id":1,"googleCalendarId":"me@x","name":"Me","color":"#16a765","showOnTimeline":true,"isPrimary":true,"archived":false})
        );
    }

    #[test]
    fn meta_flags() {
        let f = fixture();
        assert!(!f.db.flag("x").unwrap());
        f.db.with_conn("t", |c| set_flag_in(c, "x")).unwrap();
        assert!(f.db.flag("x").unwrap());
    }
}
