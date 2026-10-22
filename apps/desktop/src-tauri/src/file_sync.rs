//! M9b: sync of the notes folder with the account. Day notes, timer sidecars and
//! `config.json` travel as encrypted blobs (`notes_crypto`); a file's `rev` is the SHA-256
//! of its plaintext. This module is the engine: pull what changed elsewhere, push what
//! changed here, never lose text. It talks to the server through `Remote` (HTTP in
//! `sync_client`, an in-memory server in the tests) and keeps what it last saw per file in
//! `catenga.sqlite` (`file_sync`, `meta`).
//!
//! Rules that hold everywhere below:
//! - a local file is overwritten only while it is exactly what was last synced (and the
//!   write is atomic; day files go through `DayStore`, so open editors are told);
//! - a local file is deleted only when it is exactly what was last synced;
//! - when both sides changed, both versions are kept.

use crate::app_db::AppDb;
use crate::atomic;
use crate::day_file::{self, Block};
use crate::mirror;
use crate::notes_crypto::{self, Key};
use crate::sessions;
use crate::storage::{self, DayChanged, DayStore, Replaced};
use crate::sync_client::SyncError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Bigger files are not synced (and are reported).
pub const MAX_FILE: u64 = 5 * 1024 * 1024;
/// Safety net against a server that never stops saying `hasMore`.
const MAX_PAGES: usize = 200;
const CONFLICT_MARK: &str = " (conflict ";

// ------------------------------------------------------------------------ paths

/// What a syncable path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Day(String),
    Sessions(String),
    Config,
    /// A conflict copy of a note or of the config (§ conflicts): synced like any file.
    Copy,
}

/// ` (conflict YYYY-MM-DD HHMM)` and an optional ` N`, then `ext`: what follows a copy's
/// base name. `rest` is the file name after the base.
fn is_copy_tail(rest: &str, ext: &str) -> bool {
    let Some(tail) = rest.strip_prefix(CONFLICT_MARK) else {
        return false;
    };
    let b = tail.as_bytes();
    let stamp_ok = b.len() >= 16
        && is_digits(&tail[0..4], 4)
        && b[4] == b'-'
        && is_digits(&tail[5..7], 2)
        && b[7] == b'-'
        && is_digits(&tail[8..10], 2)
        && b[10] == b' '
        && is_digits(&tail[11..15], 4)
        && b[15] == b')';
    if !stamp_ok {
        return false;
    }
    let Some(after) = tail[16..].strip_suffix(ext) else {
        return false;
    };
    after.is_empty()
        || after
            .strip_prefix(' ')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// `YYYY/MM/YYYY-MM-DD.md`, `YYYY/MM/YYYY-MM-DD.sessions.json`, `config.json`, or a conflict
/// copy of a note (`YYYY/MM/YYYY-MM-DD (conflict YYYY-MM-DD HHMM)[ N].md`) or of the config
/// (`config (conflict YYYY-MM-DD HHMM)[ N].json`), exactly.
/// Anything else (temp files, other files, odd separators) is `None`.
pub fn classify(path: &str) -> Option<Kind> {
    if path == "config.json" {
        return Some(Kind::Config);
    }
    if path
        .strip_prefix("config")
        .is_some_and(|r| is_copy_tail(r, ".json"))
    {
        return Some(Kind::Copy);
    }
    let mut parts = path.split('/');
    let (year, month, file) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    if file.len() > 10
        && file.is_char_boundary(10)
        && is_copy_tail(&file[10..], ".md")
        && storage::validate_date(&file[..10]).is_ok()
        && year == &file[0..4]
        && month == &file[5..7]
    {
        return Some(Kind::Copy);
    }
    let (date, sessions) = match file.strip_suffix(".sessions.json") {
        Some(d) => (d, true),
        None => (file.strip_suffix(".md")?, false),
    };
    storage::validate_date(date).ok()?;
    if year != &date[0..4] || month != &date[5..7] {
        return None;
    }
    Some(if sessions {
        Kind::Sessions(date.to_string())
    } else {
        Kind::Day(date.to_string())
    })
}

/// Pull order: notes first, then timer sidecars (whose mirror lines are rewritten into
/// the notes), then the config.
fn order(kind: &Kind) -> u8 {
    match kind {
        Kind::Day(_) => 0,
        Kind::Sessions(_) => 1,
        Kind::Config => 2,
        Kind::Copy => 3,
    }
}

fn local_path(root: &Path, path: &str) -> PathBuf {
    path.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Syncable files found in the notes folder, as relative paths; `copies` are the
/// conflict copies among them (the user has something to look at).
#[derive(Debug, Default)]
pub struct Walk {
    pub files: Vec<String>,
    pub copies: Vec<String>,
}

fn is_digits(name: &str, len: usize) -> bool {
    name.len() == len && name.bytes().all(|b| b.is_ascii_digit())
}

pub fn walk(root: &Path) -> Walk {
    let mut out = Walk::default();
    let names = |dir: &Path| -> Vec<(String, bool)> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let is_dir = e.metadata().ok()?.is_dir();
                Some((name, is_dir))
            })
            .collect()
    };
    let mut note = |rel: String, is_dir: bool| {
        if is_dir {
            return;
        }
        match classify(&rel) {
            Some(Kind::Copy) => {
                out.copies.push(rel.clone());
                out.files.push(rel);
            }
            Some(_) => out.files.push(rel),
            None => {}
        }
    };
    for (name, is_dir) in names(root) {
        if is_dir && is_digits(&name, 4) {
            for (month, month_is_dir) in names(&root.join(&name)) {
                if month_is_dir && is_digits(&month, 2) {
                    for (file, file_is_dir) in names(&root.join(&name).join(&month)) {
                        note(format!("{name}/{month}/{file}"), file_is_dir);
                    }
                }
            }
        } else {
            note(name, is_dir);
        }
    }
    out.files.sort();
    out.copies.sort();
    out
}

// ---------------------------------------------------------------- remote contract

/// One row of `GET /api/files/changes`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub path: String,
    pub rev: String,
    #[serde(default)]
    pub size: i64,
    pub deleted: bool,
    pub seq: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesPage {
    pub cursor: i64,
    pub has_more: bool,
    pub changes: Vec<Change>,
}

/// Answer of an upload or delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Put {
    /// Stored (`seq` of the change; 0 when the server does not say).
    Done(i64),
    /// The server has another version than the base: its current rev.
    Conflict(String),
}

/// The server side of the notes folder (§1.3).
pub trait Remote {
    /// The account's notes key, base64.
    async fn key(&self) -> Result<String, SyncError>;
    async fn changes(&self, cursor: i64) -> Result<ChangesPage, SyncError>;
    /// The encrypted blob; `None` when the file is missing or deleted.
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, SyncError>;
    async fn put(
        &self,
        path: &str,
        base_rev: &str,
        rev: &str,
        blob: Vec<u8>,
    ) -> Result<Put, SyncError>;
    async fn delete(&self, path: &str, base_rev: &str) -> Result<Put, SyncError>;
}

// ----------------------------------------------------------------------- the key

/// Where the notes key is cached (the OS keychain; memory in tests).
pub trait KeyStore: Sync {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, value: &str) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// The notes key of `user`: the cached one, else fetched once from the server and cached.
/// The cache remembers whose key it is, so another account never uses it.
pub async fn resolve_key<R: Remote>(
    store: &dyn KeyStore,
    remote: &R,
    user: &str,
) -> Result<Key, SyncError> {
    let cached = store.get().map_err(SyncError::Local)?;
    if let Some((owner, hex)) = cached.as_deref().and_then(|c| c.rsplit_once(':'))
        && owner == user
        && let Some(key) = notes_crypto::from_hex(hex).and_then(|b| Key::try_from(b).ok())
    {
        return Ok(key);
    }
    let key = notes_crypto::key_from_base64(&remote.key().await?).map_err(SyncError::Server)?;
    // Not caching only costs one more request next time.
    if let Err(e) = store.set(&format!("{user}:{}", notes_crypto::to_hex(&key))) {
        eprintln!("caching the notes key: {e}");
    }
    Ok(key)
}

// -------------------------------------------------------------- bookkeeping in db

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

fn cursor_key(user: &str) -> String {
    format!("files_cursor:{user}")
}

impl AppDb {
    /// Starts a notes sync for `user` and returns its pull cursor. Rows and cursor belong to
    /// one account: with another one, the folder is compared with it from scratch.
    pub fn files_begin(&self, user: &str) -> Result<i64, String> {
        self.with_conn("preparing notes sync", |conn| {
            let tx = conn.transaction()?;
            if meta_get(&tx, "files_user")?.as_deref() != Some(user) {
                tx.execute("DELETE FROM file_sync", [])?;
                tx.execute("DELETE FROM meta WHERE key = ?1", [cursor_key(user)])?;
                meta_set(&tx, "files_user", user)?;
            }
            let cursor = meta_get(&tx, &cursor_key(user))?.and_then(|c| c.parse().ok());
            tx.commit()?;
            Ok(cursor.unwrap_or(0))
        })
    }

    /// Records the notes folder the sync rows belong to. When it differs from the last one
    /// (or none was recorded), forgets the rows and every account's cursor so the next pass
    /// compares the new folder with the server from scratch (never a delete). Returns
    /// whether it reset. Must run before any sync pass.
    pub fn files_adopt_root(&self, root: &str) -> Result<bool, String> {
        self.with_conn("preparing notes sync", |conn| {
            let tx = conn.transaction()?;
            if meta_get(&tx, "files_root")?.as_deref() == Some(root) {
                return Ok(false);
            }
            tx.execute("DELETE FROM file_sync", [])?;
            tx.execute("DELETE FROM meta WHERE key LIKE 'files_cursor:%'", [])?;
            meta_set(&tx, "files_root", root)?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn files_save_cursor(&self, user: &str, cursor: i64) -> Result<(), String> {
        self.with_conn("saving notes cursor", |conn| {
            meta_set(conn, &cursor_key(user), &cursor.to_string())
        })
    }

    /// The rev last synced for `path` (`None`: never, or gone).
    pub fn file_synced(&self, path: &str) -> Result<Option<String>, String> {
        self.with_conn("reading notes sync state", |conn| {
            conn.query_row(
                "SELECT synced_rev FROM file_sync WHERE path = ?1",
                [path],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
        })
    }

    pub fn file_mark(&self, path: &str, rev: &str, seq: i64) -> Result<(), String> {
        self.with_conn("saving notes sync state", |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO file_sync(path, synced_rev, remote_seq) VALUES (?1, ?2, ?3)",
                params![path, rev, seq],
            )
            .map(drop)
        })
    }

    pub fn file_forget(&self, path: &str) -> Result<(), String> {
        self.with_conn("saving notes sync state", |conn| {
            conn.execute("DELETE FROM file_sync WHERE path = ?1", [path])
                .map(drop)
        })
    }

    pub fn file_paths(&self) -> Result<Vec<String>, String> {
        self.with_conn("reading notes sync state", |conn| {
            conn.prepare("SELECT path FROM file_sync WHERE synced_rev IS NOT NULL ORDER BY path")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
    }
}

// ------------------------------------------------------------------- the engine

/// Everything a pass needs.
pub struct Env<'a> {
    pub root: &'a Path,
    pub store: &'a DayStore,
    pub db: &'a AppDb,
    pub key: &'a Key,
    pub user: &'a str,
    /// `YYYY-MM-DD HHMM` for conflict copies.
    pub stamp: &'a str,
}

/// What one pass did.
#[derive(Debug, Default)]
pub struct FilesOutcome {
    pub pushed: usize,
    pub pulled: usize,
    pub removed: usize,
    /// Conflict copies written in this pass (relative paths).
    pub conflicts: Vec<String>,
    /// Days the frontend must reload.
    pub days_changed: Vec<DayChanged>,
    pub config_changed: bool,
    /// Files over the size limit, not synced.
    pub skipped: Vec<String>,
    /// Files that could not be synced, with why. The rest went on.
    pub failed: Vec<String>,
    /// Files still waiting to be uploaded.
    pub pending: usize,
    /// Something was postponed (a file changed under us): the cursor stays put.
    incomplete: bool,
}

struct Local {
    bytes: Vec<u8>,
    rev: String,
}

fn read_local(root: &Path, path: &str) -> Result<Option<Local>, String> {
    let full = local_path(root, path);
    match fs::read(&full) {
        Ok(bytes) => Ok(Some(Local {
            rev: notes_crypto::rev_of(&bytes),
            bytes,
        })),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {path}: {e}")),
    }
}

/// The user's text of a day file, without the app-owned timer lines.
fn text_of(bytes: &[u8]) -> Option<(String, Vec<Block>)> {
    let file = day_file::parse(std::str::from_utf8(bytes).ok()?);
    let blocks = file
        .blocks
        .into_iter()
        .map(|mut b| {
            b.body = mirror::strip(&b.body);
            b
        })
        .collect();
    Some((file.preamble, blocks))
}

/// Two day files that only differ in the timer lines, which the app regenerates from the sidecar.
fn same_text(a: &[u8], b: &[u8]) -> bool {
    matches!((text_of(a), text_of(b)), (Some(x), Some(y)) if x == y)
}

/// A free name for the other version of `path`: `2026/10/2026-10-20 (conflict 2026-10-22 1530).md`,
/// `config (conflict …).json`; ` 2`, ` 3`… are added before the extension while the name is
/// taken. The other version of a conflict copy goes next to it under the next free number.
fn conflict_path(root: &Path, path: &str, stamp: &str) -> String {
    let (stem, ext) = match path.strip_suffix(".md") {
        Some(s) => (s, ".md"),
        None => (path.strip_suffix(".json").unwrap_or(path), ".json"),
    };
    let copy = classify(path) == Some(Kind::Copy);
    // A copy keeps its own name (without its number); a file gets a new copy name.
    let base = if copy {
        let n = stem.rfind(')').map_or(stem.len(), |i| i + 1);
        stem[..n].to_string()
    } else {
        format!("{stem}{CONFLICT_MARK}{stamp})")
    };
    let mut n = if copy { 2 } else { 1 };
    loop {
        let more = if n == 1 {
            String::new()
        } else {
            format!(" {n}")
        };
        let rel = format!("{base}{more}{ext}");
        if !local_path(root, &rel).exists() {
            return rel;
        }
        n += 1;
    }
}

/// Failure of one step: the whole pass stops (network, server), or just this file (its
/// text is in `Soft`) and the rest goes on.
enum Fail {
    Hard(SyncError),
    Soft(String),
}

impl From<SyncError> for Fail {
    fn from(e: SyncError) -> Fail {
        Fail::Hard(e)
    }
}

fn soft<T>(r: Result<T, String>) -> Result<T, Fail> {
    r.map_err(Fail::Soft)
}

struct Pass<'a, 'b, R: Remote> {
    env: &'a Env<'b>,
    remote: &'a R,
    out: &'a mut FilesOutcome,
}

impl<R: Remote> Pass<'_, '_, R> {
    fn synced(&self, path: &str) -> Result<Option<String>, Fail> {
        soft(self.env.db.file_synced(path))
    }

    fn mark(&self, path: &str, rev: &str, seq: i64) -> Result<(), Fail> {
        soft(self.env.db.file_mark(path, rev, seq))
    }

    /// Downloads and decrypts `path`. `None`: it changed on the server meanwhile (try again later).
    async fn download(&mut self, path: &str, rev: &str) -> Result<Option<Vec<u8>>, Fail> {
        let Some(blob) = self.remote.get(path).await? else {
            self.out.incomplete = true;
            return Ok(None);
        };
        let plain = soft(notes_crypto::decrypt(self.env.key, path, &blob))?;
        if notes_crypto::rev_of(&plain) != rev {
            self.out.incomplete = true;
            return Ok(None);
        }
        Ok(Some(plain))
    }

    /// Writes the downloaded `bytes` over the local file, only while it still is `expect`.
    /// `false`: it changed meanwhile.
    fn write_local(
        &mut self,
        kind: &Kind,
        path: &str,
        expect: Option<&str>,
        bytes: &[u8],
    ) -> Result<bool, Fail> {
        let env = self.env;
        match kind {
            Kind::Day(date) => match soft(env.store.replace_synced(date, expect, Some(bytes)))? {
                Replaced::Done(changed) => {
                    self.out.days_changed.extend(changed);
                    Ok(true)
                }
                Replaced::Changed => Ok(false),
            },
            Kind::Sessions(date) => {
                if !soft(sessions::replace_synced(
                    env.root,
                    date,
                    expect,
                    Some(bytes),
                ))? {
                    return Ok(false);
                }
                self.rewrite_mirror(date);
                Ok(true)
            }
            Kind::Config | Kind::Copy => {
                let full = local_path(env.root, path);
                let now = soft(read_local(env.root, path))?;
                if now.as_ref().map(|l| l.rev.as_str()) != expect {
                    return Ok(false);
                }
                soft(atomic::write_atomic(&full, bytes))?;
                self.out.config_changed |= *kind == Kind::Config;
                Ok(true)
            }
        }
    }

    /// The day's timer lines follow its sidecar.
    fn rewrite_mirror(&mut self, date: &str) {
        match self.env.store.rewrite_mirror(date) {
            Ok(changed) => self.out.days_changed.extend(changed),
            Err(e) => eprintln!("updating the timer lines of {date}: {e}"),
        }
    }

    /// Deletes the local file, only while it still is `expect`. `false`: it changed meanwhile.
    fn delete_local(&mut self, kind: &Kind, path: &str, expect: &str) -> Result<bool, Fail> {
        let env = self.env;
        match kind {
            Kind::Day(date) => match soft(env.store.replace_synced(date, Some(expect), None))? {
                Replaced::Done(changed) => {
                    self.out.days_changed.extend(changed);
                    Ok(true)
                }
                Replaced::Changed => Ok(false),
            },
            Kind::Sessions(date) => {
                soft(sessions::replace_synced(env.root, date, Some(expect), None))
            }
            Kind::Config | Kind::Copy => {
                let now = soft(read_local(env.root, path))?;
                match now {
                    Some(l) if l.rev == expect => {
                        soft(
                            fs::remove_file(local_path(env.root, path))
                                .map_err(|e| format!("deleting {path}: {e}")),
                        )?;
                        self.out.config_changed |= *kind == Kind::Config;
                        Ok(true)
                    }
                    Some(_) => Ok(false),
                    None => Ok(true),
                }
            }
        }
    }

    /// The server has `rev` for `path` (or deleted it): bring the local file in line, or
    /// keep both versions when both changed.
    async fn apply_remote(
        &mut self,
        path: &str,
        rev: &str,
        deleted: bool,
        seq: i64,
    ) -> Result<(), Fail> {
        let Some(kind) = classify(path) else {
            return Ok(());
        };
        let synced = self.synced(path)?;
        let local = soft(read_local(self.env.root, path))?;
        let local_rev = local.as_ref().map(|l| l.rev.as_str());
        let db = self.env.db;
        if deleted {
            // Gone on the server: delete here only a file nobody changed since the last sync;
            // a changed one stays and is uploaded again as new.
            if let (Some(l), Some(s)) = (local_rev, synced.as_deref())
                && l == s
                && !self.delete_local(&kind, path, s)?
            {
                self.out.incomplete = true;
                return Ok(());
            } else if local_rev.is_some() && local_rev == synced.as_deref() {
                self.out.removed += 1;
            }
            return soft(db.file_forget(path));
        }
        if synced.as_deref() == Some(rev) {
            return self.mark(path, rev, seq);
        }
        match local_rev {
            Some(l) if l == rev => self.mark(path, rev, seq),
            None => self.pull(&kind, path, rev, seq, None).await,
            Some(l) if Some(l) == synced.as_deref() => {
                let expect = l.to_string();
                self.pull(&kind, path, rev, seq, Some(&expect)).await
            }
            Some(_) => {
                let local = local.expect("matched Some above");
                self.conflict(&kind, path, rev, seq, &local).await
            }
        }
    }

    /// Replaces the local file (`expect`: what it must still be) by the server's version.
    async fn pull(
        &mut self,
        kind: &Kind,
        path: &str,
        rev: &str,
        seq: i64,
        expect: Option<&str>,
    ) -> Result<(), Fail> {
        let Some(plain) = self.download(path, rev).await? else {
            return Ok(());
        };
        if !self.write_local(kind, path, expect, &plain)? {
            self.out.incomplete = true;
            return Ok(());
        }
        self.out.pulled += 1;
        self.mark(path, rev, seq)
    }

    /// Both sides changed. Sessions merge; notes and config keep the local file and put the
    /// server's version next to it. Either way the local result is what gets uploaded, on
    /// top of the server's version (`synced_rev` = its rev).
    async fn conflict(
        &mut self,
        kind: &Kind,
        path: &str,
        rev: &str,
        seq: i64,
        local: &Local,
    ) -> Result<(), Fail> {
        let Some(remote) = self.download(path, rev).await? else {
            return Ok(());
        };
        let env = self.env;
        match kind {
            Kind::Sessions(date) => {
                soft(sessions::merge_remote(env.root, date, &remote))?;
                self.rewrite_mirror(date);
            }
            Kind::Day(_) if same_text(&local.bytes, &remote) => {}
            Kind::Day(_) | Kind::Config | Kind::Copy => {
                let copy = conflict_path(env.root, path, env.stamp);
                soft(atomic::write_atomic(&local_path(env.root, &copy), &remote))?;
                self.out.conflicts.push(copy);
            }
        }
        self.mark(path, rev, seq)
    }

    async fn pull_all(&mut self) -> Result<Option<i64>, Fail> {
        let mut cursor = soft(self.env.db.files_begin(self.env.user))?;
        let mut changes: Vec<Change> = Vec::new();
        for _ in 0..MAX_PAGES {
            let page = self.remote.changes(cursor).await?;
            changes.extend(page.changes);
            cursor = page.cursor;
            if !page.has_more {
                break;
            }
        }
        // The newest change per path, notes before sidecars before config.
        changes.sort_by_key(|c| c.seq);
        let mut latest: Vec<Change> = Vec::new();
        for c in changes {
            latest.retain(|l| l.path != c.path);
            latest.push(c);
        }
        latest.sort_by_key(|c| classify(&c.path).map_or(u8::MAX, |k| order(&k)));
        for c in latest {
            let result = self.apply_remote(&c.path, &c.rev, c.deleted, c.seq).await;
            self.record(&c.path, result)?;
        }
        Ok(Some(cursor))
    }

    /// A soft failure of one file is recorded and the pass goes on; a hard one stops it.
    fn record(&mut self, path: &str, result: Result<(), Fail>) -> Result<(), SyncError> {
        match result {
            Ok(()) => Ok(()),
            Err(Fail::Hard(e)) => Err(e),
            Err(Fail::Soft(msg)) => {
                let prefix = format!("{path}: ");
                if !self.out.failed.iter().any(|f| f.starts_with(&prefix)) {
                    self.out.failed.push(format!("{prefix}{msg}"));
                }
                Ok(())
            }
        }
    }

    async fn push_all(&mut self) -> Result<(), SyncError> {
        let found = walk(self.env.root);
        let rows = match self.env.db.file_paths() {
            Ok(r) => r,
            Err(e) => return Err(SyncError::Local(e)),
        };
        let paths: BTreeSet<String> = found.files.into_iter().chain(rows).collect();
        for path in paths {
            let result = self.push_one(&path).await;
            self.record(&path, result)?;
        }
        Ok(())
    }

    async fn push_one(&mut self, path: &str) -> Result<(), Fail> {
        let synced = self.synced(path)?;
        let meta = fs::metadata(local_path(self.env.root, path)).ok();
        if let Some(m) = &meta
            && m.len() > MAX_FILE
        {
            self.out.skipped.push(path.to_string());
            return Ok(());
        }
        let local = soft(read_local(self.env.root, path))?;
        match local {
            Some(l) if synced.as_deref() == Some(l.rev.as_str()) => Ok(()),
            Some(l) => self.upload(path, &l, synced.as_deref()).await,
            None if synced.is_some() => {
                let base = synced.unwrap_or_default();
                match self.remote.delete(path, &base).await? {
                    Put::Done(_) => {
                        self.out.pushed += 1;
                        soft(self.env.db.file_forget(path))
                    }
                    // Changed on the server meanwhile: the server's version comes back here.
                    Put::Conflict(cur) => self.apply_remote(path, &cur, false, 0).await,
                }
            }
            None => soft(self.env.db.file_forget(path)),
        }
    }

    async fn upload(&mut self, path: &str, local: &Local, base: Option<&str>) -> Result<(), Fail> {
        let mut local_rev = local.rev.clone();
        let mut bytes = local.bytes.clone();
        let mut base = base.unwrap_or_default().to_string();
        for attempt in 0..2 {
            let blob = soft(notes_crypto::encrypt(self.env.key, path, &bytes))?;
            match self.remote.put(path, &base, &local_rev, blob).await? {
                Put::Done(seq) => {
                    self.out.pushed += 1;
                    return self.mark(path, &local_rev, seq);
                }
                Put::Conflict(cur) if attempt == 0 => {
                    // Someone uploaded first: settle with their version, then try on top of it.
                    self.apply_remote(path, &cur, false, 0).await?;
                    let Some(now) = soft(read_local(self.env.root, path))? else {
                        return Ok(());
                    };
                    if self.synced(path)?.as_deref() == Some(now.rev.as_str()) {
                        return Ok(());
                    }
                    local_rev = now.rev;
                    bytes = now.bytes;
                    base = self.synced(path)?.unwrap_or_default();
                }
                Put::Conflict(_) => {
                    self.out.pending += 1;
                    self.out.incomplete = true;
                }
            }
        }
        Ok(())
    }
}

/// One notes sync: pull what changed on the server, then push what changed here.
/// Network/server failures stop the pass (nothing is lost, the next pass resumes);
/// a file that cannot be synced is reported in `failed` and the others go on.
/// What was done so far is in `out` even when this returns an error (the caller must
/// still tell the frontend about the days that changed).
pub async fn sync_files_into<R: Remote>(
    env: &Env<'_>,
    remote: &R,
    out: &mut FilesOutcome,
) -> Result<(), SyncError> {
    let mut pass = Pass { env, remote, out };
    let cursor = match pass.pull_all().await {
        Ok(c) => c,
        Err(Fail::Hard(e)) => return Err(e),
        Err(Fail::Soft(m)) => return Err(SyncError::Local(m)),
    };
    pass.push_all().await?;
    let found = walk(env.root);
    pass.out.pending += pass.out.failed.len();
    pass.out.skipped.retain(|p| found.files.contains(p));
    if let Some(cursor) = cursor
        && pass.out.failed.is_empty()
        && !pass.out.incomplete
    {
        env.db
            .files_save_cursor(env.user, cursor)
            .map_err(SyncError::Local)?;
    }
    Ok(())
}

#[cfg(test)]
pub async fn sync_files_once<R: Remote>(
    env: &Env<'_>,
    remote: &R,
) -> Result<FilesOutcome, SyncError> {
    let mut out = FilesOutcome::default();
    sync_files_into(env, remote, &mut out).await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;
    use crate::sync::tests::{Dev, clock, dev};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    const KEY: Key = [9; 32];
    const D1: &str = "2026-10-20";
    const D1_MD: &str = "2026/10/2026-10-20.md";
    const D1_SESS: &str = "2026/10/2026-10-20.sessions.json";
    const D2_MD: &str = "2026/10/2026-10-21.md";

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    // ---------------------------------------------------------------- fake server

    struct Entry {
        blob: Vec<u8>,
        rev: String,
        deleted: bool,
        seq: i64,
    }

    #[derive(Default)]
    struct Server {
        files: BTreeMap<String, Entry>,
        counter: i64,
        page: usize,
        /// Run once before the next `put` (another computer sneaking in).
        before_put: Option<(String, Vec<u8>)>,
        calls: Vec<String>,
        offline: bool,
    }

    impl Server {
        fn store(&mut self, path: &str, plain: &[u8]) {
            self.counter += 1;
            let blob = notes_crypto::encrypt(&KEY, path, plain).unwrap();
            self.files.insert(
                path.into(),
                Entry {
                    blob,
                    rev: notes_crypto::rev_of(plain),
                    deleted: false,
                    seq: self.counter,
                },
            );
        }

        fn remove(&mut self, path: &str) {
            self.counter += 1;
            let e = self.files.get_mut(path).unwrap();
            e.deleted = true;
            e.blob.clear();
            e.seq = self.counter;
        }

        fn plain(&self, path: &str) -> Vec<u8> {
            let e = &self.files[path];
            notes_crypto::decrypt(&KEY, path, &e.blob).unwrap()
        }

        fn live(&self) -> Vec<String> {
            self.files
                .iter()
                .filter(|(_, e)| !e.deleted)
                .map(|(p, _)| p.clone())
                .collect()
        }
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<Server>>);

    fn fake(page: usize) -> Fake {
        Fake(Arc::new(Mutex::new(Server {
            page,
            ..Server::default()
        })))
    }

    impl Fake {
        fn s(&self) -> std::sync::MutexGuard<'_, Server> {
            self.0.lock().unwrap()
        }

        fn check_online(&self) -> Result<(), SyncError> {
            if self.s().offline {
                return Err(SyncError::Offline("offline".into()));
            }
            Ok(())
        }

        fn guard(&self, path: &str, base: &str, rev: &str) -> Option<String> {
            let s = self.s();
            match s.files.get(path) {
                Some(e) if !e.deleted && e.rev != base && e.rev != rev => Some(e.rev.clone()),
                _ => None,
            }
        }
    }

    impl Remote for Fake {
        async fn key(&self) -> Result<String, SyncError> {
            self.check_online()?;
            self.s().calls.push("key".into());
            Ok("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".into())
        }

        async fn changes(&self, cursor: i64) -> Result<ChangesPage, SyncError> {
            self.check_online()?;
            let mut s = self.s();
            s.calls.push(format!("changes {cursor}"));
            let mut newer: Vec<_> = s.files.iter().filter(|(_, e)| e.seq > cursor).collect();
            newer.sort_by_key(|(_, e)| e.seq);
            let has_more = newer.len() > s.page;
            newer.truncate(s.page);
            let last = newer.last().map_or(cursor, |(_, e)| e.seq);
            Ok(ChangesPage {
                cursor: last,
                has_more,
                changes: newer
                    .iter()
                    .map(|(p, e)| Change {
                        path: (*p).clone(),
                        rev: e.rev.clone(),
                        size: i64::try_from(e.blob.len()).unwrap(),
                        deleted: e.deleted,
                        seq: e.seq,
                    })
                    .collect(),
            })
        }

        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, SyncError> {
            self.check_online()?;
            let mut s = self.s();
            s.calls.push(format!("get {path}"));
            Ok(s.files
                .get(path)
                .filter(|e| !e.deleted)
                .map(|e| e.blob.clone()))
        }

        async fn put(
            &self,
            path: &str,
            base: &str,
            rev: &str,
            blob: Vec<u8>,
        ) -> Result<Put, SyncError> {
            self.check_online()?;
            let sneak = self.s().before_put.take();
            if let Some((p, plain)) = sneak {
                self.s().store(&p, &plain);
            }
            self.s().calls.push(format!("put {path} base={base}"));
            if let Some(cur) = self.guard(path, base, rev) {
                return Ok(Put::Conflict(cur));
            }
            let mut s = self.s();
            if s.files
                .get(path)
                .is_some_and(|e| !e.deleted && e.rev == rev)
            {
                return Ok(Put::Done(s.files[path].seq));
            }
            s.counter += 1;
            let seq = s.counter;
            s.files.insert(
                path.into(),
                Entry {
                    blob,
                    rev: rev.into(),
                    deleted: false,
                    seq,
                },
            );
            Ok(Put::Done(seq))
        }

        async fn delete(&self, path: &str, base: &str) -> Result<Put, SyncError> {
            self.check_online()?;
            self.s().calls.push(format!("delete {path} base={base}"));
            if let Some(cur) = self.guard(path, base, "") {
                return Ok(Put::Conflict(cur));
            }
            let mut s = self.s();
            if s.files.contains_key(path) {
                s.remove(path);
            }
            Ok(Put::Done(s.counter))
        }
    }

    // ---------------------------------------------------------------- a computer

    struct Computer {
        _tmp: TempDir,
        store: DayStore,
        d: Dev,
    }

    fn computer() -> Computer {
        let tmp = TempDir::new();
        let store = DayStore::new(fs::canonicalize(&tmp.0).unwrap());
        Computer {
            _tmp: tmp,
            store,
            d: dev(&clock()),
        }
    }

    impl Computer {
        fn root(&self) -> &Path {
            self.store.root()
        }

        fn write(&self, rel: &str, text: &str) {
            let p = local_path(self.root(), rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        fn read(&self, rel: &str) -> Option<String> {
            fs::read_to_string(local_path(self.root(), rel)).ok()
        }

        fn rm(&self, rel: &str) {
            fs::remove_file(local_path(self.root(), rel)).unwrap();
        }

        fn sync_as(&self, remote: &Fake, user: &str, stamp: &str) -> FilesOutcome {
            let env = Env {
                root: self.root(),
                store: &self.store,
                db: &self.d.db,
                key: &KEY,
                user,
                stamp,
            };
            run(sync_files_once(&env, remote)).unwrap()
        }

        fn sync(&self, remote: &Fake) -> FilesOutcome {
            self.sync_as(remote, "u1", "2026-10-22 1530")
        }

        fn names(&self) -> Vec<String> {
            walk(self.root()).files
        }
    }

    fn day(text: &str) -> String {
        format!("## 09:00–10:00 · A\n{text}\n")
    }

    // ----------------------------------------------------------------------- tests

    #[test]
    fn only_the_three_file_shapes_are_syncable() {
        assert_eq!(classify(D1_MD), Some(Kind::Day(D1.into())));
        assert_eq!(classify(D1_SESS), Some(Kind::Sessions(D1.into())));
        assert_eq!(classify("config.json"), Some(Kind::Config));
        for bad in [
            "",
            "/2026/10/2026-10-20.md",
            "2026/10/2026-10-20 (conflict 2026-10-22 1530.md",
            "2026/10/2026-10-20 (conflict 2026-10-22 153).md",
            "2026/10/2026-10-20 (conflict 2026-10-22 1530) x.md",
            "2026/10/2026-10-20 (conflict 2026-10-22 1530) .md",
            "2026/10/2026-10-20 (conflict 2026-10-22 1530).json",
            "2026/10/2026-10-21 (conflict 2026-10-22 1530).sessions.json",
            "2026/11/2026-10-20 (conflict 2026-10-22 1530).md",
            "2026/10/2026-13-20 (conflict 2026-10-22 1530).md",
            "2026/10/2026-10-20 (conflict 2026-10-22 1530)/x.md",
            "config (conflict 2026-10-22 1530).md",
            "config (conflict 2026-10-22 1530) 2x.json",
            "config.json (conflict 2026-10-22 1530).json",
            "2026/10/2026-10-20.md.tmp",
            "2026/10/.2026-10-20.md.catenga-tmp-1-0",
            "2026/10/2026-10-20.sessions.json.bak",
            "2026/11/2026-10-20.md",
            "2025/10/2026-10-20.md",
            "2026/10/2026-13-20.md",
            "../2026/10/2026-10-20.md",
            "2026/10/../10/2026-10-20.md",
            "2026\\10\\2026-10-20.md",
            "2026/10/x/2026-10-20.md",
            "goals.json.imported",
            "config.json.bak",
            "notes/config.json",
            "attachments/a.png",
        ] {
            assert_eq!(classify(bad), None, "{bad:?}");
        }
        for copy in [
            "2026/10/2026-10-20 (conflict 2026-10-22 1530).md",
            "2026/10/2026-10-20 (conflict 2026-10-22 0930) 2.md",
            "2026/10/2026-10-20 (conflict 2026-10-22 0930) 13.md",
            "config (conflict 2026-10-22 1530).json",
            "config (conflict 2026-10-22 1530) 3.json",
        ] {
            assert_eq!(classify(copy), Some(Kind::Copy), "{copy:?}");
        }
    }

    #[test]
    fn walk_finds_syncable_files_and_conflict_copies_only() {
        let c = computer();
        c.write(D1_MD, "a");
        c.write(D1_SESS, "{}");
        c.write("config.json", "{}");
        c.write("2026/10/2026-10-20 (conflict 2026-10-22 1530).md", "x");
        c.write("config (conflict 2026-10-22 1530).json", "x");
        c.write("goals.json.imported", "x");
        c.write("2026/10/.2026-10-20.md.catenga-tmp-1-0", "x");
        c.write("2026/10/notes.txt", "x");
        c.write("misc/2026-10-20.md", "x");
        fs::create_dir_all(local_path(c.root(), "2026/10/2026-10-22.md")).unwrap();
        let w = walk(c.root());
        // Conflict copies are synced files too.
        assert_eq!(
            w.files,
            [
                "2026/10/2026-10-20 (conflict 2026-10-22 1530).md",
                "2026/10/2026-10-20.md",
                D1_SESS,
                "config (conflict 2026-10-22 1530).json",
                "config.json"
            ]
        );
        assert_eq!(
            w.copies,
            [
                "2026/10/2026-10-20 (conflict 2026-10-22 1530).md",
                "config (conflict 2026-10-22 1530).json"
            ]
        );
    }

    #[test]
    fn new_changed_and_deleted_local_files_are_pushed() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("one"));
        c.write(D1_SESS, "{\"version\":1,\"sessions\":[]}\n");
        c.write("config.json", "{}\n");
        c.write("2026/10/2026-10-20 (conflict 2026-10-22 1530).md", "never");
        c.write("goals.json.imported", "never");
        let out = c.sync(&srv);
        assert_eq!(out.pushed, 4, "three files and the conflict copy");
        assert_eq!(srv.s().live().len(), 4, "{:?}", srv.s().live());
        assert_eq!(srv.s().plain(D1_MD), day("one").as_bytes());
        // The server never sees plaintext.
        assert!(!srv.s().files[D1_MD].blob.windows(3).any(|w| w == b"one"));
        assert_eq!(
            srv.s().files[D1_MD].rev,
            notes_crypto::rev_of(day("one").as_bytes())
        );
        // Unchanged: nothing is sent.
        let calls = srv.s().calls.len();
        let out = c.sync(&srv);
        assert_eq!(out.pushed, 0);
        assert!(
            srv.s().calls[calls..]
                .iter()
                .all(|c| c.starts_with("changes"))
        );
        // Changed: pushed with the previous rev as base.
        c.write(D1_MD, &day("two"));
        c.sync(&srv);
        assert_eq!(srv.s().plain(D1_MD), day("two").as_bytes());
        let base = notes_crypto::rev_of(day("one").as_bytes());
        assert!(srv.s().calls.contains(&format!("put {D1_MD} base={base}")));
        // Deleted: a tombstone, with the synced rev as base.
        c.rm(D1_MD);
        let out = c.sync(&srv);
        assert_eq!(out.pushed, 1);
        assert!(srv.s().files[D1_MD].deleted);
        let base = notes_crypto::rev_of(day("two").as_bytes());
        assert!(
            srv.s()
                .calls
                .contains(&format!("delete {D1_MD} base={base}"))
        );
        assert_eq!(c.d.db.file_synced(D1_MD).unwrap(), None);
    }

    #[test]
    fn switching_to_an_empty_folder_downloads_everything_and_never_deletes() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("one"));
        c.write(D1_SESS, "{\"version\":1,\"sessions\":[]}\n");
        c.sync(&srv);
        assert!(c.d.db.files_adopt_root("/old").unwrap());
        assert!(!c.d.db.files_adopt_root("/old").unwrap());
        // Rows exist for /old again (as after a normal pass there).
        c.d.db.file_mark(D1_MD, "r", 1).unwrap();
        srv.s().calls.clear();
        // The owner picks an empty folder: same database, same account, new root.
        assert!(c.d.db.files_adopt_root("/new").unwrap());
        assert_eq!(c.d.db.file_paths().unwrap(), Vec::<String>::new());
        let tmp = TempDir::new();
        let store = DayStore::new(fs::canonicalize(&tmp.0).unwrap());
        let env = Env {
            root: store.root(),
            store: &store,
            db: &c.d.db,
            key: &KEY,
            user: "u1",
            stamp: "2026-10-22 1530",
        };
        let out = run(sync_files_once(&env, &srv)).unwrap();
        assert_eq!(out.pushed, 0);
        let calls = srv.s().calls.clone();
        assert!(
            calls
                .iter()
                .all(|c| !c.starts_with("delete") && !c.starts_with("put")),
            "{calls:?}"
        );
        assert_eq!(srv.s().live().len(), 2, "{:?}", srv.s().live());
        assert_eq!(
            fs::read_to_string(local_path(store.root(), D1_MD)).unwrap(),
            day("one")
        );
        assert!(local_path(store.root(), D1_SESS).exists());
    }

    #[test]
    fn without_the_reset_an_empty_folder_would_delete_remote_files() {
        // Documents why the reset exists: stale rows plus an empty folder read as deletions.
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("one"));
        c.sync(&srv);
        let tmp = TempDir::new();
        let store = DayStore::new(fs::canonicalize(&tmp.0).unwrap());
        let env = Env {
            root: store.root(),
            store: &store,
            db: &c.d.db,
            key: &KEY,
            user: "u1",
            stamp: "2026-10-22 1530",
        };
        run(sync_files_once(&env, &srv)).unwrap();
        assert!(srv.s().calls.iter().any(|c| c.starts_with("delete")));
    }

    #[test]
    fn new_changed_and_removed_remote_files_are_pulled_through_the_day_store() {
        let (c, srv) = (computer(), fake(500));
        srv.s().store(D1_MD, day("remote").as_bytes());
        srv.s().store("config.json", b"{\"tagColors\":{}}\n");
        let out = c.sync(&srv);
        assert_eq!(out.pulled, 2);
        assert_eq!(c.read(D1_MD).unwrap(), day("remote"));
        assert_eq!(c.read("config.json").unwrap(), "{\"tagColors\":{}}\n");
        // The open editor is told, and the frontend's rev is the new one.
        assert_eq!(out.days_changed.len(), 1);
        assert_eq!(out.days_changed[0].date, D1);
        assert_eq!(out.days_changed[0].rev, c.store.load(D1).unwrap().rev);
        assert!(out.config_changed);
        // Unchanged locally + changed remotely: overwritten.
        srv.s().store(D1_MD, day("remote 2").as_bytes());
        let out = c.sync(&srv);
        assert_eq!(c.read(D1_MD).unwrap(), day("remote 2"));
        assert_eq!(out.pushed, 0, "what we pulled is not pushed back");
        // Removed remotely while unchanged here: removed here too, and the editor is told.
        srv.s().remove(D1_MD);
        let out = c.sync(&srv);
        assert_eq!(c.read(D1_MD), None);
        assert_eq!(out.removed, 1);
        assert_eq!(out.days_changed[0].rev, "");
        assert_eq!(c.names(), ["config.json"]);
    }

    #[test]
    fn a_remote_delete_never_removes_a_file_changed_here_and_it_is_uploaded_again() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("v1"));
        c.sync(&srv);
        srv.s().remove(D1_MD);
        c.write(D1_MD, &day("v1 plus my edit"));
        let out = c.sync(&srv);
        assert_eq!(c.read(D1_MD).unwrap(), day("v1 plus my edit"));
        assert_eq!(out.removed, 0);
        assert_eq!(out.pushed, 1);
        assert_eq!(srv.s().plain(D1_MD), day("v1 plus my edit").as_bytes());
        assert!(!srv.s().files[D1_MD].deleted);
    }

    #[test]
    fn both_changed_keeps_the_local_note_writes_the_remote_next_to_it_and_uploads_local() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_MD, &day("base"));
        a.sync(&srv);
        b.sync(&srv);
        // Offline edits on both.
        a.write(D1_MD, &day("A's edit"));
        b.write(D1_MD, &day("B's edit"));
        a.sync(&srv);
        let out = b.sync_as(&srv, "u1", "2026-10-22 1600");
        // B keeps its text; A's version sits next to it.
        assert_eq!(b.read(D1_MD).unwrap(), day("B's edit"));
        let copy = "2026/10/2026-10-20 (conflict 2026-10-22 1600).md";
        assert_eq!(b.read(copy).unwrap(), day("A's edit"));
        assert_eq!(out.conflicts, [copy]);
        // B's text is what the account has now, uploaded on top of A's version.
        assert_eq!(srv.s().plain(D1_MD), day("B's edit").as_bytes());
        let a_rev = notes_crypto::rev_of(day("A's edit").as_bytes());
        assert!(srv.s().calls.contains(&format!("put {D1_MD} base={a_rev}")));
        // The copy is a synced file too, and a second pass is quiet.
        assert_eq!(srv.s().live(), [copy, D1_MD]);
        assert_eq!(srv.s().plain(copy), day("A's edit").as_bytes());
        let out = b.sync(&srv);
        assert_eq!((out.pushed, out.conflicts.len()), (0, 0));
        // A takes B's text (A's own edit lives in B's conflict copy: nothing is lost).
        a.sync(&srv);
        assert_eq!(a.read(D1_MD).unwrap(), day("B's edit"));
    }

    #[test]
    fn the_config_conflicts_like_a_note_and_copies_do_not_overwrite_each_other() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write("config.json", "{\"v\":0}\n");
        a.sync(&srv);
        b.sync(&srv);
        a.write("config.json", "{\"v\":1}\n");
        b.write("config.json", "{\"v\":2}\n");
        a.sync(&srv);
        let out = b.sync(&srv);
        assert_eq!(b.read("config.json").unwrap(), "{\"v\":2}\n");
        assert_eq!(
            b.read("config (conflict 2026-10-22 1530).json").unwrap(),
            "{\"v\":1}\n"
        );
        assert_eq!(out.conflicts, ["config (conflict 2026-10-22 1530).json"]);
        // Same minute again: a number keeps the first copy.
        assert_eq!(
            conflict_path(b.root(), "config.json", "2026-10-22 1530"),
            "config (conflict 2026-10-22 1530) 2.json"
        );
        assert_eq!(
            conflict_path(b.root(), "2026/10/2026-10-20.md", "2026-10-22 0930"),
            "2026/10/2026-10-20 (conflict 2026-10-22 0930).md"
        );
    }

    #[test]
    fn a_difference_only_in_timer_lines_is_not_a_conflict() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_MD, &day("same text"));
        a.sync(&srv);
        b.sync(&srv);
        a.write(
            D1_MD,
            &format!(
                "{}\n⏱ 09:05–09:50 · 45 min · free\n",
                day("same text").trim_end()
            ),
        );
        b.write(
            D1_MD,
            &format!(
                "{}\n⏱ 09:10–09:40 · 30 min · free\n",
                day("same text").trim_end()
            ),
        );
        a.sync(&srv);
        let out = b.sync(&srv);
        assert!(out.conflicts.is_empty());
        assert_eq!(b.names(), [D1_MD]);
        assert!(b.read(D1_MD).unwrap().contains("09:10–09:40"));
    }

    fn sidecar(sessions: &[(&str, &str)]) -> String {
        let list: Vec<_> = sessions
            .iter()
            .map(|(s, e)| {
                format!(
                    "{{\"start\":\"{s}\",\"end\":\"{e}\",\"mode\":\"free\",\"block\":{{\"start\":\"09:00\",\"end\":\"10:00\",\"title\":\"A\",\"tags\":[]}}}}"
                )
            })
            .collect();
        format!("{{\"version\":1,\"sessions\":[{}]}}\n", list.join(","))
    }

    const S1: (&str, &str) = ("2026-10-20T09:00:00-03:00", "2026-10-20T09:30:00-03:00");
    const S2: (&str, &str) = ("2026-10-20T11:00:00-03:00", "2026-10-20T11:30:00-03:00");
    const S3: (&str, &str) = ("2026-10-20T15:00:00-03:00", "2026-10-20T15:30:00-03:00");

    #[test]
    fn sessions_merge_on_conflict_and_both_computers_end_up_with_the_union() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_SESS, &sidecar(&[S1]));
        a.sync(&srv);
        b.sync(&srv);
        a.write(D1_SESS, &sidecar(&[S1, S2]));
        b.write(D1_SESS, &sidecar(&[S1, S3]));
        a.sync(&srv);
        let out = b.sync(&srv);
        assert!(out.conflicts.is_empty(), "sessions merge, they don't fork");
        assert_eq!(b.names(), [D1_SESS]);
        let starts = |text: &str| {
            let v: serde_json::Value = serde_json::from_str(text).unwrap();
            v["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["start"].as_str().unwrap()[11..16].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            starts(&b.read(D1_SESS).unwrap()),
            ["09:00", "11:00", "15:00"]
        );
        assert_eq!(
            starts(&String::from_utf8(srv.s().plain(D1_SESS)).unwrap()),
            ["09:00", "11:00", "15:00"]
        );
        a.sync(&srv);
        assert_eq!(
            starts(&a.read(D1_SESS).unwrap()),
            ["09:00", "11:00", "15:00"]
        );
    }

    #[test]
    fn a_pulled_sidecar_rewrites_the_timer_lines_of_its_day() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_MD, "## 09:00–10:00 · A\nnote\n");
        a.sync(&srv);
        b.sync(&srv);
        a.write(D1_SESS, &sidecar(&[S1]));
        a.sync(&srv);
        let out = b.sync(&srv);
        let md = b.read(D1_MD).unwrap();
        assert!(md.contains("⏱ 09:00–09:30 · 30 min · free"), "{md}");
        assert!(out.days_changed.iter().any(|d| d.date == D1));
    }

    #[test]
    fn first_sync_with_existing_notes_on_both_sides() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("same")); // on both, same
        c.write(D2_MD, &day("local 2")); // on both, different
        c.write("2026/10/2026-10-22.md", &day("local only"));
        srv.s().store(D1_MD, day("same").as_bytes());
        srv.s().store(D2_MD, day("remote 2").as_bytes());
        srv.s()
            .store("2026/10/2026-10-23.md", day("remote only").as_bytes());
        let out = c.sync(&srv);
        // Same rev: just marked synced, nothing moved.
        assert_eq!(
            c.d.db.file_synced(D1_MD).unwrap().unwrap(),
            notes_crypto::rev_of(day("same").as_bytes())
        );
        assert!(!srv.s().calls.iter().any(|c| c == &format!("get {D1_MD}")));
        // Different: both kept, local uploaded.
        assert_eq!(c.read(D2_MD).unwrap(), day("local 2"));
        assert_eq!(
            c.read("2026/10/2026-10-21 (conflict 2026-10-22 1530).md")
                .unwrap(),
            day("remote 2")
        );
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(srv.s().plain(D2_MD), day("local 2").as_bytes());
        // Local-only uploaded, remote-only downloaded.
        assert_eq!(
            srv.s().plain("2026/10/2026-10-22.md"),
            day("local only").as_bytes()
        );
        assert_eq!(c.read("2026/10/2026-10-23.md").unwrap(), day("remote only"));
    }

    #[test]
    fn a_stale_base_409_is_settled_with_the_servers_version_and_retried() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("v1"));
        c.sync(&srv);
        c.write(D1_MD, &day("mine"));
        // Another computer uploads after our pull, before our push.
        srv.s().before_put = Some((D1_MD.into(), day("theirs").as_bytes().to_vec()));
        let out = c.sync(&srv);
        assert_eq!(c.read(D1_MD).unwrap(), day("mine"));
        assert_eq!(
            c.read("2026/10/2026-10-20 (conflict 2026-10-22 1530).md")
                .unwrap(),
            day("theirs")
        );
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(srv.s().plain(D1_MD), day("mine").as_bytes());
        let theirs = notes_crypto::rev_of(day("theirs").as_bytes());
        assert!(
            srv.s()
                .calls
                .contains(&format!("put {D1_MD} base={theirs}"))
        );
        assert_eq!(out.pending, 0);
    }

    #[test]
    fn a_local_delete_loses_to_a_newer_remote_version() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("v1"));
        c.sync(&srv);
        c.rm(D1_MD);
        srv.s().store(D1_MD, day("v2 from elsewhere").as_bytes());
        c.sync(&srv);
        assert_eq!(c.read(D1_MD).unwrap(), day("v2 from elsewhere"));
        assert!(!srv.s().files[D1_MD].deleted);
    }

    #[test]
    fn two_computers_converge_through_one_server() {
        let (a, b, srv) = (computer(), computer(), fake(2));
        a.write(D1_MD, &day("from A"));
        a.write(D1_SESS, &sidecar(&[S1]));
        a.write("config.json", "{\"a\":1}\n");
        b.write(D2_MD, &day("from B"));
        for _ in 0..2 {
            a.sync(&srv);
            b.sync(&srv);
        }
        assert_eq!(a.names(), b.names());
        assert_eq!(a.names().len(), 4, "{:?}", a.names());
        for rel in a.names() {
            if rel != D1_MD && rel != D1_SESS {
                assert_eq!(a.read(&rel), b.read(&rel), "{rel}");
            }
        }
        // Deleting on A removes it on B.
        a.rm(D2_MD);
        a.sync(&srv);
        b.sync(&srv);
        assert_eq!(b.read(D2_MD), None);
        // And editing on B reaches A.
        b.write(D1_MD, &day("B edited"));
        b.sync(&srv);
        a.sync(&srv);
        assert_eq!(a.read(D1_MD).unwrap(), day("B edited"));
        assert_eq!(a.sync(&srv).pushed + b.sync(&srv).pushed, 0);
    }

    #[test]
    fn paging_pulls_everything() {
        let (c, srv) = (computer(), fake(2));
        for d in 10..17 {
            srv.s().store(
                &format!("2026/10/2026-10-{d}.md"),
                day(&d.to_string()).as_bytes(),
            );
        }
        let out = c.sync(&srv);
        assert_eq!(out.pulled, 7);
        let changes = srv
            .s()
            .calls
            .iter()
            .filter(|c| c.starts_with("changes"))
            .count();
        assert_eq!(changes, 4);
        // The cursor was saved: the next pass starts after the last change.
        assert_eq!(c.d.db.files_begin("u1").unwrap(), 7);
    }

    #[test]
    fn a_blob_swapped_onto_another_path_is_refused_and_nothing_is_overwritten() {
        let (c, srv) = (computer(), fake(500));
        c.write(D2_MD, &day("precious"));
        srv.s().store(D1_MD, day("remote").as_bytes());
        // The server (or an attacker) serves D1's blob under D2's name.
        let blob = srv.s().files[D1_MD].blob.clone();
        srv.s().store(D2_MD, b"x");
        srv.s().files.get_mut(D2_MD).unwrap().blob = blob;
        srv.s().files.get_mut(D2_MD).unwrap().rev = notes_crypto::rev_of(day("remote").as_bytes());
        let out = c.sync(&srv);
        assert_eq!(c.read(D2_MD).unwrap(), day("precious"));
        assert_eq!(
            c.read(D1_MD).unwrap(),
            day("remote"),
            "the honest file still syncs"
        );
        assert_eq!(out.failed.len(), 1);
        assert!(
            out.failed[0].contains("could not be decrypted"),
            "{:?}",
            out.failed
        );
        // The cursor did not move past the bad file.
        assert_eq!(c.d.db.files_begin("u1").unwrap(), 0);
    }

    #[test]
    fn too_big_files_are_skipped_and_reported() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &"x".repeat(usize::try_from(MAX_FILE).unwrap() + 1));
        c.write(D2_MD, &day("small"));
        let out = c.sync(&srv);
        assert_eq!(out.skipped, [D1_MD]);
        assert_eq!(srv.s().live(), [D2_MD]);
        assert_eq!(out.pushed, 1);
    }

    #[test]
    fn offline_leaves_files_and_state_alone_and_a_later_pass_catches_up() {
        let (c, srv) = (computer(), fake(500));
        c.write(D1_MD, &day("v1"));
        srv.s().offline = true;
        let env = Env {
            root: c.root(),
            store: &c.store,
            db: &c.d.db,
            key: &KEY,
            user: "u1",
            stamp: "s",
        };
        let err = run(sync_files_once(&env, &srv)).unwrap_err();
        assert!(matches!(err, SyncError::Offline(_)));
        assert_eq!(c.read(D1_MD).unwrap(), day("v1"));
        srv.s().offline = false;
        assert_eq!(c.sync(&srv).pushed, 1);
    }

    #[test]
    fn another_account_starts_from_scratch() {
        let (c, one, two) = (computer(), fake(500), fake(500));
        c.write(D1_MD, &day("mine"));
        c.sync_as(&one, "u1", "s");
        assert!(c.d.db.file_synced(D1_MD).unwrap().is_some());
        // Signing in to another account: its server gets the notes too.
        let out = c.sync_as(&two, "u2", "s");
        assert_eq!(out.pushed, 1);
        assert_eq!(two.s().plain(D1_MD), day("mine").as_bytes());
        assert_eq!(
            c.d.db.file_synced(D1_MD).unwrap().unwrap(),
            notes_crypto::rev_of(day("mine").as_bytes())
        );
    }

    struct Mem(Mutex<Option<String>>);

    impl KeyStore for Mem {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn set(&self, v: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(v.into());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn the_key_is_fetched_once_cached_per_account_and_removed_on_sign_out() {
        let (srv, store) = (fake(1), Mem(Mutex::new(None)));
        assert_eq!(run(resolve_key(&store, &srv, "u1")).unwrap(), KEY);
        assert_eq!(run(resolve_key(&store, &srv, "u1")).unwrap(), KEY);
        let fetched = srv.s().calls.iter().filter(|c| *c == "key").count();
        assert_eq!(fetched, 1, "second time comes from the cache");
        // Never stored in the clear next to anything but its owner.
        let stored = store.get().unwrap().unwrap();
        assert!(stored.starts_with("u1:"));
        // Another account does not use it.
        assert_eq!(run(resolve_key(&store, &srv, "u2")).unwrap(), KEY);
        assert_eq!(srv.s().calls.iter().filter(|c| *c == "key").count(), 2);
        assert!(store.get().unwrap().unwrap().starts_with("u2:"));
        // Sign out.
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
        run(resolve_key(&store, &srv, "u2")).unwrap();
        assert_eq!(srv.s().calls.iter().filter(|c| *c == "key").count(), 3);
        // A damaged cache is replaced, not trusted.
        store.set("u2:zz").unwrap();
        assert_eq!(run(resolve_key(&store, &srv, "u2")).unwrap(), KEY);
    }

    #[test]
    fn a_bad_key_from_the_server_is_an_error() {
        struct BadKey;
        impl Remote for BadKey {
            async fn key(&self) -> Result<String, SyncError> {
                Ok("AAEC".into())
            }
            async fn changes(&self, _: i64) -> Result<ChangesPage, SyncError> {
                unreachable!()
            }
            async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, SyncError> {
                unreachable!()
            }
            async fn put(&self, _: &str, _: &str, _: &str, _: Vec<u8>) -> Result<Put, SyncError> {
                unreachable!()
            }
            async fn delete(&self, _: &str, _: &str) -> Result<Put, SyncError> {
                unreachable!()
            }
        }
        let store = Mem(Mutex::new(None));
        assert!(matches!(
            run(resolve_key(&store, &BadKey, "u1")),
            Err(SyncError::Server(_))
        ));
        assert_eq!(store.get().unwrap(), None);
    }

    #[test]
    fn after_a_both_sides_edit_both_computers_end_up_with_both_texts() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_MD, &day("base"));
        a.sync(&srv);
        b.sync(&srv);
        a.write(D1_MD, &day("A's text"));
        b.write(D1_MD, &day("B's text"));
        // A syncs first, B conflicts; then each syncs again.
        for _ in 0..2 {
            a.sync_as(&srv, "u1", "2026-10-22 0930");
            b.sync_as(&srv, "u1", "2026-10-22 0930");
        }
        let copy = "2026/10/2026-10-20 (conflict 2026-10-22 0930).md";
        for c in [&a, &b] {
            assert_eq!(c.read(D1_MD).unwrap(), day("B's text"));
            assert_eq!(
                c.read(copy).unwrap(),
                day("A's text"),
                "A's text is not one disk away from loss"
            );
            assert_eq!(c.names().len(), 2);
        }
        // Quiet from now on.
        assert_eq!(a.sync(&srv).pushed + b.sync(&srv).pushed, 0);
        // The status keeps listing the copy on both.
        assert_eq!(walk(a.root()).copies, [copy]);
        assert_eq!(walk(b.root()).copies, [copy]);
    }

    #[test]
    fn deleting_a_conflict_copy_resolves_it_on_every_computer() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        a.write(D1_MD, &day("base"));
        a.sync(&srv);
        b.sync(&srv);
        a.write(D1_MD, &day("A"));
        b.write(D1_MD, &day("B"));
        for _ in 0..2 {
            a.sync(&srv);
            b.sync(&srv);
        }
        let copy = "2026/10/2026-10-20 (conflict 2026-10-22 1530).md";
        assert!(a.read(copy).is_some() && b.read(copy).is_some());
        // The user reads it on A and deletes it.
        a.rm(copy);
        a.sync(&srv);
        assert!(srv.s().files[copy].deleted);
        b.sync(&srv);
        assert_eq!(b.read(copy), None, "unchanged on B, so it goes");
        assert!(walk(b.root()).copies.is_empty());
        // A copy edited on the other computer meanwhile is not deleted there.
        let c2 = computer();
        c2.sync(&srv); // pulls nothing of it: deleted
        assert_eq!(c2.read(copy), None);
    }

    #[test]
    fn a_copy_changed_on_the_other_computer_survives_a_delete() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        let copy = "2026/10/2026-10-20 (conflict 2026-10-22 1530).md";
        a.write(copy, &day("kept version"));
        a.sync(&srv);
        b.sync(&srv);
        a.rm(copy);
        b.write(copy, &day("kept version, annotated on B"));
        a.sync(&srv);
        b.sync(&srv);
        assert_eq!(b.read(copy).unwrap(), day("kept version, annotated on B"));
        assert_eq!(
            srv.s().plain(copy),
            day("kept version, annotated on B").as_bytes()
        );
    }

    #[test]
    fn a_conflict_on_a_conflict_copy_never_recurses() {
        let (a, b, srv) = (computer(), computer(), fake(500));
        let copy = "2026/10/2026-10-20 (conflict 2026-10-22 1530).md";
        a.write(copy, &day("v0"));
        a.sync(&srv);
        b.sync(&srv);
        a.write(copy, &day("A edits the copy"));
        b.write(copy, &day("B edits the copy"));
        a.sync_as(&srv, "u1", "2026-10-23 1000");
        let out = b.sync_as(&srv, "u1", "2026-10-23 1000");
        // Local stays; the remote one is next to it under the next number, not a new stamp.
        let next = "2026/10/2026-10-20 (conflict 2026-10-22 1530) 2.md";
        assert_eq!(b.read(copy).unwrap(), day("B edits the copy"));
        assert_eq!(b.read(next).unwrap(), day("A edits the copy"));
        assert_eq!(out.conflicts, [next]);
        for _ in 0..2 {
            a.sync(&srv);
            b.sync(&srv);
        }
        for c in [&a, &b] {
            assert_eq!(c.names().len(), 2, "{:?}", c.names());
            assert_eq!(c.read(next).unwrap(), day("A edits the copy"));
        }
        // And a third conflict takes ` 3`.
        assert_eq!(
            conflict_path(b.root(), copy, "x"),
            "2026/10/2026-10-20 (conflict 2026-10-22 1530) 3.md"
        );
    }
}
