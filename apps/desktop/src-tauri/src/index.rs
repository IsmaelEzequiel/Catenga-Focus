//! Derived SQLite index over the day files (blocks + FTS5). Never the source of
//! truth: it only reads the notes folder and can be deleted and rebuilt at any
//! time. No Tauri.

use crate::dashboard::{self, DashRange, Dashboard};
use crate::day_file;
use crate::google::CalEvent;
use crate::search::{self, HistoryKind, HistoryResult, SearchResponse, norm, norm_tag};
use crate::sessions;
use crate::storage::{date_from_path, day_path, rev_of, sidecar_date_from_path};
use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, params};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

/// Bump when the schema changes; a mismatch resets the database (it is only a cache).
pub const SCHEMA_VERSION: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexState {
    Building,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reindex {
    Indexed,
    Unchanged,
    Removed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexEvent {
    Day(String),
    /// A timer sidecar (`*.sessions.json`) of this day changed.
    Sessions(String),
    Rescan,
}

const SCHEMA: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(date TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL,
                   mtime_ns INTEGER NOT NULL, rev TEXT NOT NULL);
CREATE TABLE blocks(id INTEGER PRIMARY KEY, date TEXT NOT NULL, ord INTEGER NOT NULL,
                    start INTEGER NOT NULL, \"end\" INTEGER NOT NULL,
                    title TEXT NOT NULL, title_norm TEXT NOT NULL,
                    tags TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE block_tags(block_id INTEGER NOT NULL REFERENCES blocks(id) ON DELETE CASCADE,
                        tag TEXT NOT NULL, tag_norm TEXT NOT NULL);
CREATE INDEX blocks_date ON blocks(date);
CREATE INDEX blocks_title_norm ON blocks(title_norm);
CREATE INDEX block_tags_norm ON block_tags(tag_norm);
CREATE VIRTUAL TABLE blocks_fts USING fts5(title, tags, body, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE calendar_events(date TEXT NOT NULL, event_id TEXT NOT NULL, ord INTEGER NOT NULL,
                             start INTEGER NOT NULL, \"end\" INTEGER NOT NULL,
                             title TEXT NOT NULL, html_link TEXT,
                             calendar_id TEXT NOT NULL DEFAULT '',
                             PRIMARY KEY(date, event_id));
CREATE TABLE calendar_days(date TEXT PRIMARY KEY, fetched_at INTEGER NOT NULL);
CREATE TABLE sidecars(date TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL,
                      mtime_ns INTEGER NOT NULL);
CREATE TABLE sessions(id INTEGER PRIMARY KEY, date TEXT NOT NULL, start TEXT NOT NULL,
                      \"end\" TEXT NOT NULL, minutes INTEGER NOT NULL, mode TEXT NOT NULL,
                      block_start INTEGER NOT NULL, block_end INTEGER NOT NULL,
                      title TEXT NOT NULL, title_norm TEXT NOT NULL, tags TEXT NOT NULL,
                      project TEXT);
CREATE TABLE session_tags(session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                          tag TEXT NOT NULL, tag_norm TEXT NOT NULL);
CREATE INDEX sessions_date ON sessions(date);
CREATE INDEX sessions_title_norm ON sessions(title_norm);
CREATE INDEX session_tags_norm ON session_tags(tag_norm);
";

const BUSY_TIMEOUT: Duration = Duration::from_millis(3000);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sql_err(context: &str, e: rusqlite::Error) -> String {
    format!("{context}: {e}")
}

/// Opens (creating if needed) the database and checks it matches this schema
/// version and notes root. Any mismatch is an error so the caller can reset.
fn open_checked(db_path: &Path, root: &Path) -> Result<Connection, String> {
    if let Some(dir) = db_path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let conn = Connection::open(db_path).map_err(|e| sql_err("opening index", e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| sql_err("busy timeout", e))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| sql_err("foreign_keys", e))?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
        .map_err(|e| sql_err("journal_mode", e))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| sql_err("synchronous", e))?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| sql_err("reading user_version", e))?;
    let root_str = root.to_string_lossy().into_owned();
    if version == 0 {
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .map_err(|e| sql_err("reading schema", e))?;
        if tables != 0 {
            return Err("index database has an unknown schema".into());
        }
        conn.execute_batch(SCHEMA)
            .map_err(|e| sql_err("creating schema", e))?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('notes_root', ?1)",
            [&root_str],
        )
        .map_err(|e| sql_err("writing meta", e))?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| sql_err("setting user_version", e))?;
    } else if version != SCHEMA_VERSION {
        return Err(format!(
            "index schema version {version}, expected {SCHEMA_VERSION}"
        ));
    } else {
        let stored: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'notes_root'", [], |r| {
                r.get(0)
            })
            .map_err(|e| sql_err("reading meta", e))?;
        if stored.as_deref() != Some(root_str.as_str()) {
            return Err("index was built for another notes folder".into());
        }
    }
    Ok(conn)
}

fn remove_db_files(db_path: &Path) {
    let name = db_path.as_os_str().to_os_string();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = name.clone();
        p.push(suffix);
        let _ = fs::remove_file(PathBuf::from(p));
    }
}

/// Opens the database, deleting and recreating it once if it cannot be used.
fn open_or_reset(db_path: &Path, root: &Path) -> Result<Connection, String> {
    match open_checked(db_path, root) {
        Ok(conn) => Ok(conn),
        Err(first) => {
            eprintln!("resetting search index: {first}");
            remove_db_files(db_path);
            open_checked(db_path, root)
        }
    }
}

pub struct Index {
    db_path: PathBuf,
    root: PathBuf,
    /// The single writer.
    conn: Mutex<Option<Connection>>,
    status: Mutex<IndexStatus>,
    /// Set while a runtime reset + full sync is running, so it cannot recurse.
    resyncing: AtomicBool,
}

/// Error text plus whether the database itself is damaged (reset needed).
type Failure = (String, bool);

fn is_fatal(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::SystemIoFailure)
    )
}

/// Highlight markers must never appear in indexed text.
fn strip_markers(s: &str) -> String {
    s.replace(['\u{2}', '\u{3}'], "")
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn delete_day(tx: &Transaction, date: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM blocks_fts WHERE rowid IN (SELECT id FROM blocks WHERE date = ?1)",
        [date],
    )?;
    tx.execute("DELETE FROM blocks WHERE date = ?1", [date])?;
    tx.execute("DELETE FROM files WHERE date = ?1", [date])?;
    Ok(())
}

fn insert_day(
    tx: &Transaction,
    date: &str,
    file: &day_file::DayFile,
    (size, mtime, rev): (i64, i64, &str),
) -> rusqlite::Result<()> {
    let rel = format!("{}/{}/{date}.md", &date[0..4], &date[5..7]);
    tx.execute(
        "INSERT INTO files(date, path, size, mtime_ns, rev) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![date, rel, size, mtime, rev],
    )?;
    for (ord, b) in file.blocks.iter().enumerate() {
        let title = strip_markers(&b.title);
        let tags = strip_markers(&b.tags.join(" "));
        let body = strip_markers(&crate::mirror::strip(&b.body));
        tx.prepare_cached(
            "INSERT INTO blocks(date, ord, start, \"end\", title, title_norm, tags, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            date,
            ord as i64,
            b.start,
            b.end,
            title,
            norm(&title),
            tags,
            body
        ])?;
        let id = tx.last_insert_rowid();
        tx.prepare_cached(
            "INSERT INTO blocks_fts(rowid, title, tags, body) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![id, title, tags, body])?;
        for tag in tags.split_whitespace() {
            tx.prepare_cached(
                "INSERT INTO block_tags(block_id, tag, tag_norm) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![id, tag, norm_tag(tag)])?;
        }
    }
    Ok(())
}

/// `(date, size, mtime_ns)` of every `root/YYYY/MM/YYYY-MM-DD.md`, sorted by date.
fn scan(root: &Path) -> Vec<(String, i64, i64)> {
    scan_with(root, date_from_path)
}

/// Same for the timer sidecars `root/YYYY/MM/YYYY-MM-DD.sessions.json`.
fn scan_sidecars(root: &Path) -> Vec<(String, i64, i64)> {
    scan_with(root, sidecar_date_from_path)
}

fn scan_with(root: &Path, date_of: fn(&Path, &Path) -> Option<String>) -> Vec<(String, i64, i64)> {
    let mut found = Vec::new();
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    };
    for year in subdirs(root) {
        for month in subdirs(&year) {
            for entry in fs::read_dir(&month).into_iter().flatten().flatten() {
                let path = entry.path();
                let Some(date) = date_of(root, &path) else {
                    continue;
                };
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_file() {
                    found.push((
                        date,
                        i64::try_from(meta.len()).unwrap_or(i64::MAX),
                        mtime_ns(&meta),
                    ));
                }
            }
        }
    }
    found.sort();
    found
}

fn reindex_locked(conn: &mut Connection, date: &str, path: &Path) -> Result<Reindex, Failure> {
    let wrap = |context: &str, e: rusqlite::Error| (format!("{context}: {e}"), is_fatal(&e));
    let drop_day = |conn: &mut Connection| -> Result<(), Failure> {
        let tx = conn
            .transaction()
            .map_err(|e| wrap("starting transaction", e))?;
        delete_day(&tx, date).map_err(|e| wrap("removing day", e))?;
        tx.commit().map_err(|e| wrap("committing", e))
    };
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            drop_day(conn)?;
            if e.kind() == ErrorKind::NotFound {
                return Ok(Reindex::Removed);
            }
            eprintln!("indexing {}: {e}", path.display());
            return Ok(Reindex::Skipped);
        }
    };
    // Stat before reading: a write landing in between leaves an older mtime
    // stored, so the next sync re-reads the file.
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            drop_day(conn)?;
            if e.kind() == ErrorKind::NotFound {
                return Ok(Reindex::Removed);
            }
            eprintln!("indexing {}: {e}", path.display());
            return Ok(Reindex::Skipped);
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        eprintln!("indexing {}: file is not valid UTF-8", path.display());
        drop_day(conn)?;
        return Ok(Reindex::Skipped);
    };
    let rev = rev_of(&bytes);
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let mtime = mtime_ns(&meta);
    let stored: Option<String> = conn
        .query_row("SELECT rev FROM files WHERE date = ?1", [date], |r| {
            r.get(0)
        })
        .ok();
    if stored.as_deref() == Some(rev.as_str()) {
        conn.execute(
            "UPDATE files SET size = ?2, mtime_ns = ?3 WHERE date = ?1",
            params![date, size, mtime],
        )
        .map_err(|e| wrap("updating file stats", e))?;
        return Ok(Reindex::Unchanged);
    }
    let file = day_file::parse(text);
    let tx = conn
        .transaction()
        .map_err(|e| wrap("starting transaction", e))?;
    delete_day(&tx, date).map_err(|e| wrap("replacing day", e))?;
    insert_day(&tx, date, &file, (size, mtime, &rev)).map_err(|e| wrap("indexing day", e))?;
    tx.commit().map_err(|e| wrap("committing", e))?;
    Ok(Reindex::Indexed)
}

/// `"HH:MM"` as minutes since midnight; 0 if it does not parse.
fn hhmm_minutes(s: &str) -> i64 {
    let parse = || -> Option<i64> {
        let (h, m) = s.split_once(':')?;
        Some(h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?)
    };
    parse().unwrap_or(0)
}

fn delete_sessions(tx: &Transaction, date: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM session_tags WHERE session_id IN (SELECT id FROM sessions WHERE date = ?1)",
        [date],
    )?;
    tx.execute("DELETE FROM sessions WHERE date = ?1", [date])?;
    tx.execute("DELETE FROM sidecars WHERE date = ?1", [date])?;
    Ok(())
}

fn insert_sessions(
    tx: &Transaction,
    date: &str,
    all: &[sessions::Session],
    (size, mtime): (i64, i64),
) -> rusqlite::Result<()> {
    let rel = format!("{}/{}/{date}.sessions.json", &date[0..4], &date[5..7]);
    tx.execute(
        "INSERT INTO sidecars(date, path, size, mtime_ns) VALUES (?1, ?2, ?3, ?4)",
        params![date, rel, size, mtime],
    )?;
    for s in all {
        // Only finished sessions count; the running one shows up once stopped.
        let Some(end) = &s.end else { continue };
        let Some(seconds) = sessions::seconds_between(&s.start, end) else {
            continue;
        };
        let title = &s.block.title;
        let tags = s.block.tags.join(" ");
        tx.prepare_cached(
            "INSERT INTO sessions(date, start, \"end\", minutes, mode, block_start, block_end,
                                  title, title_norm, tags, project)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?
        .execute(params![
            date,
            s.start,
            end,
            (seconds / 60).max(0),
            s.mode.as_str(),
            hhmm_minutes(&s.block.start),
            hhmm_minutes(&s.block.end),
            title,
            norm(title),
            tags,
            s.block.project
        ])?;
        let id = tx.last_insert_rowid();
        for tag in &s.block.tags {
            tx.prepare_cached(
                "INSERT INTO session_tags(session_id, tag, tag_norm) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![id, tag, norm_tag(tag)])?;
        }
    }
    Ok(())
}

fn reindex_sessions_locked(
    conn: &mut Connection,
    date: &str,
    path: &Path,
) -> Result<Reindex, Failure> {
    let wrap = |context: &str, e: rusqlite::Error| (format!("{context}: {e}"), is_fatal(&e));
    let drop_day = |conn: &mut Connection| -> Result<(), Failure> {
        let tx = conn
            .transaction()
            .map_err(|e| wrap("starting transaction", e))?;
        delete_sessions(&tx, date).map_err(|e| wrap("removing sessions", e))?;
        tx.commit().map_err(|e| wrap("committing", e))
    };
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            drop_day(conn)?;
            if e.kind() == ErrorKind::NotFound {
                return Ok(Reindex::Removed);
            }
            eprintln!("indexing {}: {e}", path.display());
            return Ok(Reindex::Skipped);
        }
    };
    // Stat before reading, as for day files: a racing write leaves an older mtime stored.
    let all = match sessions::read_file(path) {
        Ok(all) => all,
        Err(e) => {
            eprintln!("indexing sessions of {date}: {e}");
            drop_day(conn)?;
            return Ok(Reindex::Skipped);
        }
    };
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let tx = conn
        .transaction()
        .map_err(|e| wrap("starting transaction", e))?;
    delete_sessions(&tx, date).map_err(|e| wrap("replacing sessions", e))?;
    insert_sessions(&tx, date, &all, (size, mtime_ns(&meta)))
        .map_err(|e| wrap("indexing sessions", e))?;
    tx.commit().map_err(|e| wrap("committing", e))?;
    Ok(Reindex::Indexed)
}

impl Index {
    /// Starts with status `Building` until the first `sync_all` finishes.
    pub fn open(db_path: &Path, root: &Path) -> Result<Index, String> {
        let conn = open_or_reset(db_path, root)?;
        Ok(Index {
            db_path: db_path.to_path_buf(),
            root: root.to_path_buf(),
            conn: Mutex::new(Some(conn)),
            status: Mutex::new(IndexStatus {
                state: IndexState::Building,
                done: 0,
                total: 0,
            }),
            resyncing: AtomicBool::new(false),
        })
    }

    /// A short-lived read-only connection; WAL lets it run beside the writer.
    pub fn reader(&self) -> Result<Connection, String> {
        let conn = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| sql_err("opening index for reading", e))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| sql_err("busy timeout", e))?;
        Ok(conn)
    }

    pub fn status(&self) -> IndexStatus {
        *lock(&self.status)
    }

    pub fn search(&self, input: &str) -> Result<SearchResponse, String> {
        let mut response = self.read(|conn| search::search(conn, input))?;
        response.indexing = self.status().state == IndexState::Building;
        Ok(response)
    }

    pub fn history(&self, kind: HistoryKind, value: &str) -> Result<HistoryResult, String> {
        let mut result = self.read(|conn| search::history(conn, kind, value))?;
        result.indexing = self.status().state == IndexState::Building;
        Ok(result)
    }

    /// Dashboard aggregates for `range` ending `today` (local date).
    pub fn dashboard(
        &self,
        range: DashRange,
        today: jiff::civil::Date,
    ) -> Result<Dashboard, String> {
        let mut result = self.read(|conn| dashboard::load(conn, range, today))?;
        result.indexing = self.status().state == IndexState::Building;
        Ok(result)
    }

    /// Progress of the given goals as of `today` (local date).
    pub fn goals(
        &self,
        goals: &[crate::goals::GoalView],
        today: jiff::civil::Date,
    ) -> Result<Vec<crate::goals::GoalProgress>, String> {
        self.read(|conn| crate::goals::load(conn, goals, today))
    }

    /// Every tag seen in blocks and sessions, once per normalised name (first spelling).
    pub fn all_tags(&self) -> Result<Vec<String>, String> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT MIN(tag) FROM (SELECT tag, tag_norm FROM block_tags
                                       UNION ALL SELECT tag, tag_norm FROM session_tags)
                 WHERE tag_norm <> '' GROUP BY tag_norm ORDER BY tag_norm",
            )?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
    }

    /// Runs a read query; a damaged database is reset and rebuilt, and the
    /// error is returned once.
    fn read<T>(&self, query: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.reader()?;
        match query(&conn) {
            Ok(value) => Ok(value),
            Err(e) => {
                drop(conn);
                if is_fatal(&e) {
                    self.reset_and_resync();
                }
                Err(format!("searching index: {e}"))
            }
        }
    }

    /// Brings one day in line with its file. Reads the file inside the writer
    /// lock so racing callers cannot leave an older version indexed. A damaged
    /// database is reset and rebuilt (the error is still returned once).
    pub fn reindex_day(&self, date: &str) -> Result<Reindex, String> {
        let path = day_path(&self.root, date)?;
        let result = match lock(&self.conn).as_mut() {
            Some(conn) => reindex_locked(conn, date, &path),
            None => Err(("search index unavailable".to_string(), false)),
        };
        match result {
            Ok(outcome) => Ok(outcome),
            Err((message, fatal)) => {
                if fatal {
                    self.reset_and_resync();
                }
                Err(message)
            }
        }
    }

    /// Brings one day's finished timer sessions in line with its sidecar. A
    /// missing sidecar removes the rows; an invalid one removes them too (and
    /// is logged) — it never fails the caller.
    pub fn reindex_sessions(&self, date: &str) -> Result<Reindex, String> {
        let path = sessions::sidecar_path(&self.root, date)?;
        let result = match lock(&self.conn).as_mut() {
            Some(conn) => reindex_sessions_locked(conn, date, &path),
            None => Err(("search index unavailable".to_string(), false)),
        };
        match result {
            Ok(outcome) => Ok(outcome),
            Err((message, fatal)) => {
                if fatal {
                    self.reset_and_resync();
                }
                Err(message)
            }
        }
    }

    fn reset_and_resync(&self) {
        if self.resyncing.swap(true, Ordering::SeqCst) {
            return;
        }
        eprintln!("search index damaged, rebuilding");
        match self.recover() {
            Ok(()) => {
                self.sync_all();
            }
            Err(e) => eprintln!("rebuilding search index: {e}"),
        }
        self.resyncing.store(false, Ordering::SeqCst);
    }

    /// Full reconciliation with the notes folder. Known limitation: an edit
    /// that keeps both a file's size and mtime is not noticed until that file
    /// next changes (deleting the database forces a rebuild).
    pub fn sync_all(&self) -> SyncReport {
        self.sync_all_with(&|_| {})
    }

    pub fn sync_all_with(&self, on_progress: &dyn Fn(IndexStatus)) -> SyncReport {
        let set = |status: IndexStatus| {
            *lock(&self.status) = status;
            on_progress(status);
        };
        set(IndexStatus {
            state: IndexState::Building,
            done: 0,
            total: 0,
        });
        let found = scan(&self.root);
        let total = found.len();
        let mut report = SyncReport::default();
        let mut tally = |outcome: Result<Reindex, String>| match outcome {
            Ok(Reindex::Indexed) => report.indexed += 1,
            Ok(Reindex::Unchanged) => report.unchanged += 1,
            Ok(Reindex::Removed) => report.removed += 1,
            Ok(Reindex::Skipped) => report.skipped += 1,
            Err(e) => {
                eprintln!("indexing: {e}");
                report.skipped += 1;
            }
        };
        let mut seen = HashSet::new();
        for (i, (date, size, mtime)) in found.into_iter().enumerate() {
            if self.stored_stat(&date) == Some((size, mtime)) {
                tally(Ok(Reindex::Unchanged));
            } else {
                tally(self.reindex_day(&date));
            }
            seen.insert(date);
            set(IndexStatus {
                state: IndexState::Building,
                done: i + 1,
                total,
            });
        }
        // Days indexed earlier whose file is gone (reindex_day re-checks the disk,
        // so a file that appeared meanwhile is indexed rather than dropped).
        for date in self
            .stored_dates()
            .into_iter()
            .filter(|d| !seen.contains(d))
        {
            tally(self.reindex_day(&date));
        }
        self.sync_sidecars();
        set(IndexStatus {
            state: IndexState::Ready,
            done: total,
            total,
        });
        report
    }

    /// Reconciles the timer sidecars (not part of the day-file report).
    fn sync_sidecars(&self) {
        let mut seen = HashSet::new();
        for (date, size, mtime) in scan_sidecars(&self.root) {
            if self.stored_sidecar_stat(&date) != Some((size, mtime))
                && let Err(e) = self.reindex_sessions(&date)
            {
                eprintln!("indexing sessions of {date}: {e}");
            }
            seen.insert(date);
        }
        // Sidecars indexed earlier whose file is gone.
        for date in self
            .stored_sidecar_dates()
            .into_iter()
            .filter(|d| !seen.contains(d))
        {
            if let Err(e) = self.reindex_sessions(&date) {
                eprintln!("indexing sessions of {date}: {e}");
            }
        }
    }

    fn stored_sidecar_stat(&self, date: &str) -> Option<(i64, i64)> {
        let guard = lock(&self.conn);
        guard
            .as_ref()?
            .query_row(
                "SELECT size, mtime_ns FROM sidecars WHERE date = ?1",
                [date],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    fn stored_sidecar_dates(&self) -> Vec<String> {
        let guard = lock(&self.conn);
        let Some(conn) = guard.as_ref() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare("SELECT date FROM sidecars") else {
            return Vec::new();
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    fn stored_stat(&self, date: &str) -> Option<(i64, i64)> {
        let guard = lock(&self.conn);
        guard
            .as_ref()?
            .query_row(
                "SELECT size, mtime_ns FROM files WHERE date = ?1",
                [date],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    fn stored_dates(&self) -> Vec<String> {
        let guard = lock(&self.conn);
        let Some(conn) = guard.as_ref() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare("SELECT date FROM files") else {
            return Vec::new();
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Replaces the cached Google events of `date` and records when they were fetched
    /// (unix ms), in one transaction. An empty list still marks the day as fetched.
    pub fn replace_calendar_day(
        &self,
        date: &str,
        events: &[CalEvent],
        fetched_at: i64,
    ) -> Result<(), String> {
        let mut guard = lock(&self.conn);
        let conn = guard.as_mut().ok_or("search index unavailable")?;
        let tx = conn
            .transaction()
            .map_err(|e| sql_err("starting calendar write", e))?;
        let write = |tx: &Transaction| -> rusqlite::Result<()> {
            tx.execute("DELETE FROM calendar_events WHERE date = ?1", [date])?;
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO calendar_events(date, event_id, ord, start, \"end\", title, html_link, calendar_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (ord, e) in events.iter().enumerate() {
                insert.execute(params![
                    date,
                    e.id,
                    ord as i64,
                    e.start,
                    e.end,
                    e.title,
                    e.html_link,
                    e.calendar_id
                ])?;
            }
            drop(insert);
            tx.execute(
                "INSERT OR REPLACE INTO calendar_days(date, fetched_at) VALUES (?1, ?2)",
                params![date, fetched_at],
            )?;
            Ok(())
        };
        write(&tx).map_err(|e| sql_err("writing calendar cache", e))?;
        tx.commit().map_err(|e| sql_err("saving calendar cache", e))
    }

    /// The cached events of `date` and their fetch time, or `None` if never fetched.
    pub fn calendar_day(&self, date: &str) -> Result<Option<(Vec<CalEvent>, i64)>, String> {
        let conn = self.reader()?;
        let read = || -> rusqlite::Result<Option<(Vec<CalEvent>, i64)>> {
            let fetched: Option<i64> = conn
                .query_row(
                    "SELECT fetched_at FROM calendar_days WHERE date = ?1",
                    [date],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            let Some(fetched) = fetched else {
                return Ok(None);
            };
            let mut stmt = conn.prepare(
                "SELECT event_id, start, \"end\", title, html_link, calendar_id FROM calendar_events
                 WHERE date = ?1 ORDER BY ord",
            )?;
            let events = stmt
                .query_map([date], |r| {
                    Ok(CalEvent {
                        id: r.get(0)?,
                        start: r.get(1)?,
                        end: r.get(2)?,
                        title: r.get(3)?,
                        html_link: r.get(4)?,
                        calendar_id: r.get(5)?,
                        project_id: None,
                        color: String::new(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some((events, fetched)))
        };
        read().map_err(|e| sql_err("reading calendar cache", e))
    }

    /// Forgets every cached Google event (on disconnect).
    pub fn clear_calendar(&self) -> Result<(), String> {
        let guard = lock(&self.conn);
        let conn = guard.as_ref().ok_or("search index unavailable")?;
        conn.execute_batch("DELETE FROM calendar_events; DELETE FROM calendar_days;")
            .map_err(|e| sql_err("clearing calendar cache", e))
    }

    /// Deletes the database files and reopens an empty one. Callers follow up with `sync_all`.
    pub fn recover(&self) -> Result<(), String> {
        let mut guard = lock(&self.conn);
        // Windows cannot delete a file that is still open.
        drop(guard.take());
        remove_db_files(&self.db_path);
        *guard = Some(open_checked(&self.db_path, &self.root)?);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use crate::config::test_util::TempDir;
    use std::fs;

    pub struct Env {
        pub tmp: TempDir,
        pub root: PathBuf,
        pub db: PathBuf,
    }

    impl Env {
        pub fn new() -> Env {
            let tmp = TempDir::new();
            let root = tmp.0.join("notes");
            fs::create_dir_all(&root).unwrap();
            let db = tmp.0.join("cache").join("index.sqlite");
            Env { tmp, root, db }
        }

        pub fn open(&self) -> Index {
            Index::open(&self.db, &self.root).unwrap()
        }

        pub fn write(&self, date: &str, text: &str) -> PathBuf {
            let p = crate::storage::day_path(&self.root, date).unwrap();
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, text).unwrap();
            p
        }
    }

    pub fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use std::fs;

    fn user_version(db: &Path) -> i64 {
        let c = Connection::open(db).unwrap();
        c.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    fn ev(id: &str, start: u32, end: u32, title: &str) -> CalEvent {
        CalEvent {
            id: id.into(),
            start,
            end,
            title: title.into(),
            calendar_id: String::new(),
            html_link: Some(format!("https://l/{id}")),
            project_id: None,
            color: String::new(),
        }
    }

    #[test]
    fn calendar_day_is_none_until_stored() {
        let env = Env::new();
        let idx = env.open();
        assert_eq!(idx.calendar_day("2026-10-21").unwrap(), None);
    }

    #[test]
    fn calendar_replace_and_read_round_trip() {
        let env = Env::new();
        let idx = env.open();
        let events = vec![ev("a", 540, 600, "A"), ev("b", 600, 660, "B")];
        idx.replace_calendar_day("2026-10-21", &events, 1234)
            .unwrap();
        assert_eq!(
            idx.calendar_day("2026-10-21").unwrap(),
            Some((events, 1234))
        );
        assert_eq!(idx.calendar_day("2026-10-22").unwrap(), None);
    }

    #[test]
    fn calendar_replace_overwrites_the_day_only() {
        let env = Env::new();
        let idx = env.open();
        idx.replace_calendar_day("2026-10-21", &[ev("a", 1, 2, "A")], 1)
            .unwrap();
        idx.replace_calendar_day("2026-10-22", &[ev("z", 3, 4, "Z")], 1)
            .unwrap();
        idx.replace_calendar_day("2026-10-21", &[ev("b", 5, 6, "B")], 2)
            .unwrap();
        let (events, at) = idx.calendar_day("2026-10-21").unwrap().unwrap();
        assert_eq!((events.len(), events[0].id.as_str(), at), (1, "b", 2));
        assert_eq!(idx.calendar_day("2026-10-22").unwrap().unwrap().0.len(), 1);
    }

    #[test]
    fn calendar_empty_day_is_cached_as_fetched() {
        let env = Env::new();
        let idx = env.open();
        idx.replace_calendar_day("2026-10-21", &[], 7).unwrap();
        assert_eq!(idx.calendar_day("2026-10-21").unwrap(), Some((vec![], 7)));
    }

    #[test]
    fn calendar_clear_removes_everything() {
        let env = Env::new();
        let idx = env.open();
        idx.replace_calendar_day("2026-10-21", &[ev("a", 1, 2, "A")], 1)
            .unwrap();
        idx.clear_calendar().unwrap();
        assert_eq!(idx.calendar_day("2026-10-21").unwrap(), None);
    }

    #[test]
    fn calendar_survives_reopen_but_not_a_v1_database() {
        let env = Env::new();
        {
            let idx = env.open();
            idx.replace_calendar_day("2026-10-21", &[ev("a", 1, 2, "A")], 1)
                .unwrap();
        }
        assert!(env.open().calendar_day("2026-10-21").unwrap().is_some());
        // An old v1 database is reset and rebuilt with the calendar tables.
        let env = Env::new();
        fs::create_dir_all(env.db.parent().unwrap()).unwrap();
        {
            let c = Connection::open(&env.db).unwrap();
            c.execute_batch("CREATE TABLE meta(key TEXT, value TEXT); PRAGMA user_version = 1;")
                .unwrap();
        }
        let idx = env.open();
        assert_eq!(user_version(&env.db), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 4);
        assert_eq!(idx.calendar_day("2026-10-21").unwrap(), None);
    }

    #[test]
    fn open_creates_schema_and_sets_user_version() {
        let env = Env::new();
        let idx = env.open();
        assert_eq!(user_version(&env.db), SCHEMA_VERSION);
        let r = idx.reader().unwrap();
        for table in ["meta", "files", "blocks", "block_tags", "blocks_fts"] {
            let n = count(
                &r,
                &format!("SELECT count(*) FROM sqlite_master WHERE name = '{table}'"),
            );
            assert_eq!(n, 1, "{table}");
        }
    }

    #[test]
    fn open_with_other_user_version_resets_database() {
        let env = Env::new();
        fs::create_dir_all(env.db.parent().unwrap()).unwrap();
        {
            let c = Connection::open(&env.db).unwrap();
            c.execute_batch("CREATE TABLE old(x); PRAGMA user_version = 99;")
                .unwrap();
        }
        let idx = env.open();
        assert_eq!(user_version(&env.db), SCHEMA_VERSION);
        let r = idx.reader().unwrap();
        assert_eq!(
            count(&r, "SELECT count(*) FROM sqlite_master WHERE name = 'old'"),
            0
        );
    }

    #[test]
    fn open_with_other_notes_root_resets_database() {
        let env = Env::new();
        env.write("2026-10-20", "## 09:00–10:00 · A\nnote\n");
        {
            let idx = env.open();
            idx.sync_all();
            assert_eq!(
                count(&idx.reader().unwrap(), "SELECT count(*) FROM blocks"),
                1
            );
        }
        let other = env.tmp.0.join("other");
        fs::create_dir_all(&other).unwrap();
        let idx = Index::open(&env.db, &other).unwrap();
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM blocks"),
            0
        );
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM files"),
            0
        );
    }

    #[test]
    fn open_with_same_root_keeps_data() {
        let env = Env::new();
        env.write("2026-10-20", "## 09:00–10:00 · A\nnote\n");
        {
            let idx = env.open();
            idx.sync_all();
        }
        let idx = env.open();
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM blocks"),
            1
        );
    }

    #[test]
    fn open_replaces_corrupt_db_file() {
        let env = Env::new();
        fs::create_dir_all(env.db.parent().unwrap()).unwrap();
        fs::write(
            &env.db,
            b"this is definitely not a sqlite database, just garbage bytes ".repeat(100),
        )
        .unwrap();
        let idx = env.open();
        assert_eq!(user_version(&env.db), SCHEMA_VERSION);
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM blocks"),
            0
        );
    }

    #[test]
    fn open_failure_is_reported_not_panicking() {
        let env = Env::new();
        let blocker = env.tmp.0.join("a-file");
        fs::write(&blocker, "x").unwrap();
        let result = Index::open(&blocker.join("sub").join("index.sqlite"), &env.root);
        assert!(result.is_err());
    }

    const A: &str = "## 09:00–10:00 · Alpha #one\nalphaword note\n";

    fn fts(idx: &Index, word: &str) -> i64 {
        count(
            &idx.reader().unwrap(),
            &format!("SELECT count(*) FROM blocks_fts WHERE blocks_fts MATCH '\"{word}\"'"),
        )
    }

    fn block_count(idx: &Index) -> i64 {
        count(&idx.reader().unwrap(), "SELECT count(*) FROM blocks")
    }

    #[test]
    fn sync_all_indexes_every_day_file_across_months() {
        let env = Env::new();
        env.write("2026-10-20", A);
        env.write(
            "2026-11-02",
            "## 10:00–11:00 · B\nbetaword\n## 12:00–13:00 · C\ngammaword\n",
        );
        env.write("2025-01-05", "## 08:00–09:00 · D\ndeltaword\n");
        let idx = env.open();
        let report = idx.sync_all();
        assert_eq!(
            report,
            SyncReport {
                indexed: 3,
                unchanged: 0,
                removed: 0,
                skipped: 0
            }
        );
        assert_eq!(block_count(&idx), 4);
        for w in ["alphaword", "betaword", "gammaword", "deltaword"] {
            assert_eq!(fts(&idx, w), 1, "{w}");
        }
        assert_eq!(idx.status().state, IndexState::Ready);
    }

    #[test]
    fn sync_all_ignores_non_day_files() {
        let env = Env::new();
        env.write("2026-10-20", A);
        let junk = [
            "config.json",
            ".catenga-tmp-x",
            "notes.md",
            "2026/10/2026-10-20.md.swp",
            "2026/2026-10-20.md",
            "2026/11/2026-10-21.md",
            "2026/10/2026-13-40.md",
            "other/10/2026-10-22.md",
        ];
        for j in junk {
            let p = env.root.join(j);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "## 09:00–10:00 · Junk\njunkword\n").unwrap();
        }
        let idx = env.open();
        let report = idx.sync_all();
        assert_eq!(report.indexed, 1);
        assert_eq!(fts(&idx, "junkword"), 0);
        assert_eq!(block_count(&idx), 1);
    }

    #[test]
    fn sync_all_indexes_malformed_heading_text_as_body_of_previous_block() {
        let env = Env::new();
        env.write(
            "2026-10-20",
            "## 09:00–10:00 · A\nfirst\n## bad heading zebra\nmore\n",
        );
        let idx = env.open();
        idx.sync_all();
        assert_eq!(block_count(&idx), 1);
        assert_eq!(fts(&idx, "zebra"), 1);
    }

    #[test]
    fn sync_all_does_not_index_preamble() {
        let env = Env::new();
        env.write(
            "2026-10-20",
            "intropreamble text\n\n## 09:00–10:00 · A\nbody\n",
        );
        let idx = env.open();
        idx.sync_all();
        assert_eq!(fts(&idx, "intropreamble"), 0);
        assert_eq!(block_count(&idx), 1);
    }

    #[test]
    fn sync_all_skips_invalid_utf8_file_and_continues() {
        let env = Env::new();
        let bad = env.write("2026-10-19", "");
        fs::write(bad, [0x23, 0x23, 0xff, 0xfe, 0x0a]).unwrap();
        env.write("2026-10-20", A);
        let idx = env.open();
        let report = idx.sync_all();
        assert_eq!(report.indexed, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(fts(&idx, "alphaword"), 1);
    }

    #[test]
    fn sync_all_is_idempotent_second_run_reports_all_unchanged() {
        let env = Env::new();
        env.write("2026-10-20", A);
        env.write("2026-10-21", A);
        let idx = env.open();
        idx.sync_all();
        let report = idx.sync_all();
        assert_eq!(
            report,
            SyncReport {
                indexed: 0,
                unchanged: 2,
                removed: 0,
                skipped: 0
            }
        );
        assert_eq!(block_count(&idx), 2);
    }

    #[test]
    fn sync_all_detects_files_added_modified_removed_while_closed() {
        let env = Env::new();
        let keep = env.write("2026-10-20", A);
        env.write("2026-10-21", "## 09:00–10:00 · Gone\ngoneword\n");
        env.write("2026-10-22", "## 09:00–10:00 · Same\nsameword\n");
        {
            let idx = env.open();
            idx.sync_all();
        }
        fs::write(
            keep,
            "## 09:00–10:00 · Alpha\nchangedword and longer text\n",
        )
        .unwrap();
        fs::remove_file(crate::storage::day_path(&env.root, "2026-10-21").unwrap()).unwrap();
        env.write("2026-10-23", "## 09:00–10:00 · New\nnewword\n");
        let idx = env.open();
        let report = idx.sync_all();
        assert_eq!(
            report,
            SyncReport {
                indexed: 2,
                unchanged: 1,
                removed: 1,
                skipped: 0
            }
        );
        assert_eq!(fts(&idx, "alphaword"), 0);
        assert_eq!(fts(&idx, "changedword"), 1);
        assert_eq!(fts(&idx, "goneword"), 0);
        assert_eq!(fts(&idx, "newword"), 1);
        assert_eq!(fts(&idx, "sameword"), 1);
    }

    #[test]
    fn sync_all_removes_rows_when_whole_month_folder_deleted() {
        let env = Env::new();
        env.write("2026-10-20", A);
        env.write("2026-11-20", "## 09:00–10:00 · B\nbetaword\n");
        let idx = env.open();
        idx.sync_all();
        fs::remove_dir_all(env.root.join("2026").join("10")).unwrap();
        let report = idx.sync_all();
        assert_eq!(report.removed, 1);
        assert_eq!(block_count(&idx), 1);
        assert_eq!(fts(&idx, "alphaword"), 0);
        assert_eq!(
            count(
                &idx.reader().unwrap(),
                "SELECT count(*) FROM block_tags WHERE tag_norm = 'one'"
            ),
            0
        );
    }

    #[test]
    fn reindex_day_replaces_previous_blocks_and_fts_rows() {
        let env = Env::new();
        let p = env.write("2026-10-20", A);
        let idx = env.open();
        assert_eq!(idx.reindex_day("2026-10-20").unwrap(), Reindex::Indexed);
        assert_eq!(fts(&idx, "alphaword"), 1);
        fs::write(
            p,
            "## 09:00–10:00 · New #two\nnewword\n## 11:00–12:00 · More\nmoreword\n",
        )
        .unwrap();
        assert_eq!(idx.reindex_day("2026-10-20").unwrap(), Reindex::Indexed);
        assert_eq!(fts(&idx, "alphaword"), 0);
        assert_eq!(fts(&idx, "newword"), 1);
        assert_eq!(block_count(&idx), 2);
        let r = idx.reader().unwrap();
        assert_eq!(count(&r, "SELECT count(*) FROM block_tags"), 1);
        assert_eq!(count(&r, "SELECT count(*) FROM blocks_fts"), 2);
    }

    #[test]
    fn reindex_day_with_unchanged_content_is_noop() {
        let env = Env::new();
        env.write("2026-10-20", A);
        let idx = env.open();
        idx.reindex_day("2026-10-20").unwrap();
        assert_eq!(idx.reindex_day("2026-10-20").unwrap(), Reindex::Unchanged);
        assert_eq!(block_count(&idx), 1);
    }

    #[test]
    fn reindex_day_missing_file_removes_rows() {
        let env = Env::new();
        let p = env.write("2026-10-20", A);
        let idx = env.open();
        idx.reindex_day("2026-10-20").unwrap();
        fs::remove_file(p).unwrap();
        assert_eq!(idx.reindex_day("2026-10-20").unwrap(), Reindex::Removed);
        assert_eq!(block_count(&idx), 0);
        assert_eq!(fts(&idx, "alphaword"), 0);
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM files"),
            0
        );
    }

    #[test]
    fn reindex_day_rejects_invalid_date() {
        let env = Env::new();
        let idx = env.open();
        assert!(idx.reindex_day("../etc").is_err());
    }

    #[test]
    fn reindex_day_rewritten_same_content_new_mtime_is_unchanged() {
        let env = Env::new();
        let p = env.write("2026-10-20", A);
        let idx = env.open();
        idx.sync_all();
        let f = fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        drop(f);
        assert_eq!(idx.reindex_day("2026-10-20").unwrap(), Reindex::Unchanged);
        // The refreshed mtime means the next sync does not even re-read the file.
        assert_eq!(idx.sync_all().unchanged, 1);
    }

    #[test]
    fn reindex_after_storage_save_day_makes_new_text_searchable() {
        let env = Env::new();
        let idx = env.open();
        let block = crate::day_file::Block {
            start: 540,
            end: 600,
            title: "Saved".into(),
            tags: vec!["t".into()],
            body: "\nsavedword\n".into(),
        };
        crate::storage::save_day(&env.root, "2026-10-20", &[block], "").unwrap();
        assert_eq!(fts(&idx, "savedword"), 0);
        idx.reindex_day("2026-10-20").unwrap();
        assert_eq!(fts(&idx, "savedword"), 1);
    }

    #[test]
    fn index_strips_highlight_marker_chars_from_text() {
        let env = Env::new();
        env.write(
            "2026-10-20",
            "## 09:00–10:00 · Ti\u{2}tle\u{3}\nbo\u{2}dy\u{3}word\n",
        );
        let idx = env.open();
        idx.sync_all();
        let r = idx.reader().unwrap();
        let (title, body): (String, String) = r
            .query_row("SELECT title, body FROM blocks", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "Title");
        assert!(!body.contains('\u{2}') && !body.contains('\u{3}'));
        assert!(body.contains("bodyword"));
        // The file itself is untouched.
        let on_disk =
            fs::read_to_string(crate::storage::day_path(&env.root, "2026-10-20").unwrap()).unwrap();
        assert!(on_disk.contains('\u{2}'));
    }

    #[test]
    fn index_queries_report_indexing_until_first_sync_finishes() {
        let env = Env::new();
        env.write("2026-10-20", A);
        let idx = env.open();
        assert!(idx.search("alpha").unwrap().indexing);
        idx.sync_all();
        let r = idx.search("alpha").unwrap();
        assert!(!r.indexing);
        assert_eq!(r.hits.len(), 1);
        let h = idx.history(HistoryKind::Tag, "one").unwrap();
        assert!(!h.indexing);
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn index_query_on_damaged_database_resets_and_reports_once() {
        let env = Env::new();
        env.write("2026-10-20", A);
        let idx = env.open();
        idx.sync_all();
        // Empty the WAL so readers go straight to the main file, then damage it.
        let checkpoint = lock(&idx.conn).as_ref().unwrap().query_row(
            "PRAGMA wal_checkpoint(TRUNCATE)",
            [],
            |r| r.get::<_, i64>(0),
        );
        assert_eq!(checkpoint.unwrap(), 0);
        fs::write(&env.db, b"garbage".repeat(2000)).unwrap();
        let first = idx.search("alpha");
        assert!(first.is_err());
        assert_eq!(idx.search("alpha").unwrap().hits.len(), 1);
    }

    #[test]
    fn recover_deletes_files_and_reopens_empty() {
        let env = Env::new();
        env.write("2026-10-20", A);
        let idx = env.open();
        idx.sync_all();
        assert_eq!(block_count(&idx), 1);
        idx.recover().unwrap();
        assert!(env.db.exists());
        assert_eq!(block_count(&idx), 0);
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM files"),
            0
        );
        // And it keeps working afterwards.
        assert_eq!(idx.sync_all().indexed, 1);
        assert_eq!(block_count(&idx), 1);
    }

    #[test]
    fn concurrent_reindex_and_reads_do_not_error() {
        let env = Env::new();
        let p1 = env.write("2026-10-20", A);
        let p2 = env.write("2026-10-21", "## 09:00–10:00 · B\nbetaword\n");
        let idx = env.open();
        idx.sync_all();
        std::thread::scope(|s| {
            for t in 0..4 {
                let idx = &idx;
                let (p1, p2) = (&p1, &p2);
                s.spawn(move || {
                    for i in 0..25 {
                        let date = if (t + i) % 2 == 0 {
                            "2026-10-20"
                        } else {
                            "2026-10-21"
                        };
                        if i % 5 == 0 {
                            let p = if date == "2026-10-20" { p1 } else { p2 };
                            fs::write(p, format!("## 09:00–10:00 · T{t}\nround{i}word\n")).unwrap();
                        }
                        idx.reindex_day(date).unwrap();
                    }
                });
            }
            for _ in 0..2 {
                let idx = &idx;
                s.spawn(move || {
                    for _ in 0..50 {
                        let r = idx.reader().unwrap();
                        let n = count(&r, "SELECT count(*) FROM blocks");
                        assert!((0..=2).contains(&n));
                    }
                });
            }
        });
        idx.reindex_day("2026-10-20").unwrap();
        idx.reindex_day("2026-10-21").unwrap();
        let r = idx.reader().unwrap();
        let on_disk = fs::read_to_string(&p1).unwrap();
        let title: String = r
            .query_row(
                "SELECT title FROM blocks WHERE date = '2026-10-20'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(on_disk.contains(&format!("· {title}\n")));
        assert_eq!(block_count(&idx), 2);
    }

    #[test]
    fn sync_all_reports_progress_monotonically() {
        let env = Env::new();
        for d in ["2026-10-20", "2026-10-21", "2026-10-22"] {
            env.write(d, A);
        }
        let idx = env.open();
        let seen = Mutex::new(Vec::new());
        idx.sync_all_with(&|st| seen.lock().unwrap().push(st));
        let seen = seen.into_inner().unwrap();
        assert!(seen.len() >= 2);
        assert_eq!(seen[0].state, IndexState::Building);
        assert!(seen.windows(2).all(|w| w[0].done <= w[1].done));
        let last = seen.last().unwrap();
        assert_eq!(last.state, IndexState::Ready);
        assert_eq!((last.done, last.total), (3, 3));
    }

    #[test]
    fn index_body_has_no_mirror_lines() {
        let env = Env::new();
        env.write(
            "2026-10-20",
            "## 09:00–10:00 · A\nrealword\n\n⏱ 09:05–09:50 · 45 min · free\n",
        );
        let index = env.open();
        index.sync_all();
        let conn = Connection::open(&env.db).unwrap();
        let body: String = conn
            .query_row("SELECT body FROM blocks", [], |r| r.get(0))
            .unwrap();
        assert!(body.contains("realword"));
        assert!(!body.contains('⏱') && !body.contains("pomodoro") && !body.contains(" min "));
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM blocks_fts WHERE blocks_fts MATCH 'free'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0);
    }

    // ---- sessions (schema v3) ----

    use crate::sessions::{Mode, Session, SessionBlock};

    fn sess(start: &str, end: Option<&str>, title: &str, tags: &[&str]) -> Session {
        Session {
            start: start.into(),
            end: end.map(Into::into),
            mode: Mode::Free,
            block: SessionBlock {
                start: "09:00".into(),
                end: "10:00".into(),
                title: title.into(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                project: None,
            },
            last_seen: None,
        }
    }

    fn put(env: &Env, date: &str, s: Session) {
        crate::sessions::upsert(&env.root, date, s).unwrap();
    }

    fn rows(idx: &Index, sql: &str) -> Vec<String> {
        let conn = idx.reader().unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    const D: &str = "2026-10-20";
    const S0: &str = "2026-10-20T09:00:00-03:00";
    const S1: &str = "2026-10-20T09:45:30-03:00";

    #[test]
    fn sync_indexes_finished_sessions_with_minutes_and_tags() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "Deep Work", &["a", "#B"]));
        let idx = env.open();
        idx.sync_all();
        let conn = idx.reader().unwrap();
        let (date, minutes, mode, bs, be, title, norm_t, tags): (
            String, i64, String, i64, i64, String, String, String,
        ) = conn
            .query_row(
                "SELECT date, minutes, mode, block_start, block_end, title, title_norm, tags FROM sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            )
            .unwrap();
        assert_eq!(
            (date.as_str(), minutes, mode.as_str(), bs, be),
            (D, 45, "free", 540, 600)
        );
        assert_eq!(
            (title.as_str(), norm_t.as_str(), tags.as_str()),
            ("Deep Work", "deep work", "a #B")
        );
        assert_eq!(
            rows(&idx, "SELECT tag_norm FROM session_tags ORDER BY tag_norm"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn running_session_is_not_indexed() {
        let env = Env::new();
        put(&env, D, sess(S0, None, "A", &[]));
        let idx = env.open();
        idx.sync_all();
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM sessions"),
            0
        );
    }

    #[test]
    fn cross_midnight_session_counts_on_its_start_day() {
        let env = Env::new();
        put(
            &env,
            D,
            sess(
                "2026-10-20T23:30:00-03:00",
                Some("2026-10-21T00:30:00-03:00"),
                "A",
                &[],
            ),
        );
        let idx = env.open();
        idx.sync_all();
        assert_eq!(
            rows(&idx, "SELECT date || ' ' || minutes FROM sessions"),
            vec!["2026-10-20 60"]
        );
    }

    #[test]
    fn invalid_sidecar_removes_rows_and_does_not_fail() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "A", &["x"]));
        let idx = env.open();
        idx.sync_all();
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM sessions"),
            1
        );
        let path = crate::sessions::sidecar_path(&env.root, D).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(idx.reindex_sessions(D).unwrap(), Reindex::Skipped);
        let conn = idx.reader().unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM sessions"), 0);
        assert_eq!(count(&conn, "SELECT count(*) FROM session_tags"), 0);
        // A full sync with the bad file present also copes.
        idx.sync_all();
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM sessions"),
            0
        );
    }

    #[test]
    fn reindex_sessions_replaces_a_days_rows() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "A", &["x"]));
        let idx = env.open();
        idx.sync_all();
        put(
            &env,
            D,
            sess(S0, Some("2026-10-20T10:00:00-03:00"), "B", &["y"]),
        );
        assert_eq!(idx.reindex_sessions(D).unwrap(), Reindex::Indexed);
        assert_eq!(
            rows(&idx, "SELECT title || minutes FROM sessions"),
            vec!["B60"]
        );
        assert_eq!(rows(&idx, "SELECT tag_norm FROM session_tags"), vec!["y"]);
    }

    #[test]
    fn deleting_the_sidecar_removes_rows_on_reindex_and_on_sync() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "A", &[]));
        let idx = env.open();
        idx.sync_all();
        let path = crate::sessions::sidecar_path(&env.root, D).unwrap();
        fs::remove_file(&path).unwrap();
        idx.sync_all();
        let conn = idx.reader().unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM sessions"), 0);
        assert_eq!(count(&conn, "SELECT count(*) FROM sidecars"), 0);
    }

    #[test]
    fn sync_skips_unchanged_sidecars() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "A", &[]));
        let idx = env.open();
        idx.sync_all();
        let conn = idx.reader().unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM sidecars"), 1);
        // Mark the row so we can tell it was not rewritten by the second sync.
        drop(conn);
        {
            let guard = lock(&idx.conn);
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE sessions SET title = 'marker'", [])
                .unwrap();
        }
        idx.sync_all();
        assert_eq!(rows(&idx, "SELECT title FROM sessions"), vec!["marker"]);
    }

    #[test]
    fn index_with_sessions_equals_rebuilt_index() {
        let env = Env::new();
        env.write(D, "## 09:00–10:00 · A #x\nnote\n");
        put(&env, D, sess(S0, Some(S1), "A", &["x"]));
        put(
            &env,
            "2026-10-21",
            sess(
                "2026-10-21T08:00:00-03:00",
                Some("2026-10-21T09:00:00-03:00"),
                "B",
                &["y", "z"],
            ),
        );
        let dump = |idx: &Index| {
            rows(
                idx,
                "SELECT date||'|'||start||'|'||\"end\"||'|'||minutes||'|'||mode||'|'||block_start||'|'||block_end||'|'||title||'|'||title_norm||'|'||tags FROM sessions ORDER BY id",
            )
            .into_iter()
            .chain(rows(
                idx,
                "SELECT s.date||tag||tag_norm FROM session_tags t JOIN sessions s ON s.id = t.session_id ORDER BY s.date, tag",
            ))
            .collect::<Vec<_>>()
        };
        let first = env.open();
        first.sync_all();
        let before = dump(&first);
        assert_eq!(before.len(), 5);
        drop(first);
        remove_db_files(&env.db);
        let rebuilt = env.open();
        rebuilt.sync_all();
        assert_eq!(dump(&rebuilt), before);
    }

    #[test]
    fn session_rows_survive_reopen_and_v2_database_is_reset() {
        let env = Env::new();
        put(&env, D, sess(S0, Some(S1), "A", &[]));
        {
            let idx = env.open();
            idx.sync_all();
        }
        assert_eq!(
            count(
                &env.open().reader().unwrap(),
                "SELECT count(*) FROM sessions"
            ),
            1
        );
        let env = Env::new();
        fs::create_dir_all(env.db.parent().unwrap()).unwrap();
        {
            let c = Connection::open(&env.db).unwrap();
            c.execute_batch("CREATE TABLE meta(key TEXT, value TEXT); PRAGMA user_version = 2;")
                .unwrap();
        }
        let idx = env.open();
        assert_eq!(user_version(&env.db), SCHEMA_VERSION);
        assert_eq!(
            count(&idx.reader().unwrap(), "SELECT count(*) FROM sessions"),
            0
        );
    }

    #[test]
    fn dashboard_combines_sessions_and_notes_from_the_index() {
        let env = Env::new();
        env.write(
            "2026-10-20",
            "## 09:00–10:00 · Deep Work #a\nwrote things\n\n⏱ 09:00–09:45 · 45 min · free\n\n## 11:00–12:00 · Empty\n\n⏱ 11:00–11:30 · 30 min · free\n## 13:00–14:00 · Read\n   \n",
        );
        put(
            &env,
            "2026-10-20",
            sess(
                "2026-10-20T09:00:00-03:00",
                Some("2026-10-20T09:45:00-03:00"),
                "Deep Work",
                &["a"],
            ),
        );
        put(
            &env,
            "2026-10-21",
            sess(
                "2026-10-21T09:00:00-03:00",
                Some("2026-10-21T10:00:00-03:00"),
                "deep work",
                &["a", "b"],
            ),
        );
        put(
            &env,
            "2026-10-21",
            sess("2026-10-21T11:00:00-03:00", None, "Running", &[]),
        );
        let idx = env.open();
        idx.sync_all();
        let today: jiff::civil::Date = "2026-10-21".parse().unwrap();
        let d = idx.dashboard(DashRange::All, today).unwrap();
        assert!(!d.indexing);
        assert_eq!(
            (d.totals.minutes, d.totals.sessions, d.totals.active_days),
            (105, 2, 2)
        );
        assert_eq!((d.totals.current_streak, d.totals.longest_streak), (2, 2));
        let keys: Vec<_> = d
            .titles
            .iter()
            .map(|t| (t.key.as_str(), t.minutes, t.done_days))
            .collect();
        // "empty" and "read" have no session and no real note (mirror lines / whitespace don't count).
        assert_eq!(keys, [("deep work", 105, 2)]);
        assert_eq!(d.titles[0].label, "deep work");
        let tags: Vec<_> = d
            .tags
            .iter()
            .map(|t| (t.key.as_str(), t.minutes, t.sessions))
            .collect();
        assert_eq!(tags, [("a", 105, 2), ("b", 60, 1)]);
        assert_eq!(d.habits.len(), 1);
        assert_eq!(d.calendar.len(), 371);
    }

    #[test]
    fn dashboard_groups_untagged_sessions_last_with_their_titles() {
        let env = Env::new();
        put(
            &env,
            "2026-10-21",
            sess(
                "2026-10-21T09:00:00-03:00",
                Some("2026-10-21T10:00:00-03:00"),
                "Loose",
                &[],
            ),
        );
        put(
            &env,
            "2026-10-21",
            sess(
                "2026-10-21T11:00:00-03:00",
                Some("2026-10-21T11:10:00-03:00"),
                "Deep",
                &["a"],
            ),
        );
        let idx = env.open();
        idx.sync_all();
        let d = idx
            .dashboard(DashRange::All, "2026-10-21".parse().unwrap())
            .unwrap();
        let keys: Vec<_> = d.tags.iter().map(|t| (t.key.as_str(), t.minutes)).collect();
        assert_eq!(keys, [("a", 10), ("", 60)]);
        assert_eq!(d.tags[1].titles[0].label, "Loose");
        assert_eq!(d.tags[0].titles[0].key, "deep");
    }

    #[test]
    fn dashboard_counts_a_note_only_block_as_done() {
        let env = Env::new();
        env.write("2026-10-21", "## 09:00–10:00 · Read\nchapter 3\n");
        let idx = env.open();
        idx.sync_all();
        let d = idx
            .dashboard(DashRange::Week, "2026-10-21".parse().unwrap())
            .unwrap();
        assert_eq!(
            (
                d.totals.minutes,
                d.totals.active_days,
                d.totals.current_streak
            ),
            (0, 1, 1)
        );
        assert!(d.habits[0].days[29].note);
    }

    #[test]
    fn dashboard_while_building_flags_indexing_and_never_fails() {
        let env = Env::new();
        let idx = env.open();
        let d = idx
            .dashboard(DashRange::All, "2026-10-21".parse().unwrap())
            .unwrap();
        assert!(d.indexing);
        assert!(d.titles.is_empty());
    }

    #[test]
    fn sessions_index_their_project_calendar_id() {
        let env = Env::new();
        let mut with = sess(S0, Some(S1), "A", &[]);
        with.block.project = Some("mest@x".into());
        put(&env, D, with);
        let idx = env.open();
        idx.sync_all();
        let r = idx.reader().unwrap();
        assert_eq!(
            count(&r, "SELECT count(*) FROM sessions WHERE project = 'mest@x'"),
            1
        );
        let mut none = sess(
            "2026-10-20T11:00:00-03:00",
            Some("2026-10-20T11:30:00-03:00"),
            "B",
            &[],
        );
        none.block.project = None;
        put(&env, D, none);
        idx.reindex_sessions(D).unwrap();
        let r = idx.reader().unwrap();
        assert_eq!(
            count(&r, "SELECT count(*) FROM sessions WHERE project IS NULL"),
            1
        );
    }

    #[test]
    fn calendar_events_keep_their_calendar_id() {
        let env = Env::new();
        let idx = env.open();
        let mut a = ev("a", 540, 600, "A");
        a.calendar_id = "mest@x".into();
        let mut b = ev("b", 600, 660, "B");
        b.calendar_id = "me@x".into();
        idx.replace_calendar_day("2026-10-21", &[a, b], 1).unwrap();
        let (events, _) = idx.calendar_day("2026-10-21").unwrap().unwrap();
        let ids: Vec<_> = events.iter().map(|e| e.calendar_id.as_str()).collect();
        assert_eq!(ids, ["mest@x", "me@x"]);
    }

    #[test]
    fn all_tags_unions_blocks_and_sessions_once_per_normalised_name() {
        let env = Env::new();
        env.write(
            D,
            "## 09:00–10:00 · A #Rust #go\n\n## 11:00–12:00 · B #rust\n",
        );
        put(&env, D, sess(S0, Some(S1), "A", &["mestrado", "Go"]));
        let idx = env.open();
        idx.sync_all();
        let tags = idx.all_tags().unwrap();
        let norms: Vec<_> = tags.iter().map(|t| t.to_lowercase()).collect();
        assert_eq!(norms, ["go", "mestrado", "rust"]);
        assert_eq!(env.open().all_tags().unwrap().len(), 3);
    }
}
