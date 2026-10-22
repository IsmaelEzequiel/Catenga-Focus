//! M9a sync, Tauri side: the status the Settings line shows, when a sync runs
//! (app start, 5 s after a local change, every 5 min, "Sync now", and retries
//! with backoff) and the commands. `SyncCore` has no Tauri in it so it is tested
//! with a fake server.

use crate::account;
use crate::app_db::{AppDb, system_ms};
use crate::commands::AppDbHandle;
use crate::file_sync::{self, Env, FilesOutcome, KeyStore};
use crate::storage::{DayChanged, DayStore};
use crate::sync::Applied;
use crate::sync_client::{HttpRemote, SyncClient, SyncError, sync_once};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Listener, Manager, State};

const DEBOUNCE: Duration = Duration::from_secs(5);
/// After a change to a notes file (M9b).
const FILES_DEBOUNCE: Duration = Duration::from_secs(10);
const PERIOD: Duration = Duration::from_secs(5 * 60);
const BACKOFF_START: u64 = 30;
const BACKOFF_MAX: u64 = 300;
/// The events every local change already emits; they schedule a sync.
const CHANGE_EVENTS: [&str; 4] = [
    "goals-changed",
    "tags-changed",
    "projects-changed",
    "calendars-changed",
];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncState {
    Off,
    Idle,
    Syncing,
    Error,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub state: SyncState,
    pub last_synced_at: Option<i64>,
    pub error: Option<String>,
    pub pending: i64,
    pub files: FilesStatus,
}

/// The notes-folder half of the status (M9b).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesStatus {
    /// Notes files changed here and not synced yet.
    pub pending: i64,
    /// Conflict copies in the notes folder (both versions were kept), relative paths.
    pub conflicts: Vec<String>,
    /// Files over the size limit, not synced.
    pub skipped: Vec<String>,
    /// The server has no notes storage configured yet; app data still syncs.
    pub unavailable: bool,
}

/// What a notes sync needs besides the account: where the notes are and the key's cache.
pub struct FilesEnv<'a> {
    pub root: &'a Path,
    pub store: &'a DayStore,
    pub keys: &'a dyn KeyStore,
    /// `YYYY-MM-DD HHMM`, for conflict copies.
    pub stamp: String,
}

#[derive(Default)]
struct FilesState {
    left: i64,
    conflicts: Vec<String>,
    skipped: Vec<String>,
    unavailable: bool,
    /// Syncable files the watcher saw change since the last pass.
    touched: HashSet<String>,
}

struct Inner {
    state: SyncState,
    last_synced_at: Option<i64>,
    error: Option<String>,
    /// `(token, account)` so the account lookup happens once per token.
    identity: Option<(String, String)>,
    files: FilesState,
}

/// What one run did.
pub struct RunResult {
    pub status: SyncStatus,
    pub applied: Applied,
    /// Days the notes sync rewrote: the frontend reloads them.
    pub days_changed: Vec<DayChanged>,
    /// Failed in a way that is worth retrying later.
    pub retry: bool,
}

pub struct SyncCore {
    client: SyncClient,
    inner: Mutex<Inner>,
    running: AtomicBool,
}

impl SyncCore {
    pub fn new() -> Result<SyncCore, String> {
        Ok(SyncCore {
            client: SyncClient::new()?,
            inner: Mutex::new(Inner {
                state: SyncState::Off,
                last_synced_at: None,
                error: None,
                identity: None,
                files: FilesState::default(),
            }),
            running: AtomicBool::new(false),
        })
    }

    pub fn signed_in(&self) -> bool {
        lock(&self.inner).state != SyncState::Off
    }

    pub fn status(&self, db: Option<&AppDb>) -> SyncStatus {
        let i = lock(&self.inner);
        SyncStatus {
            state: i.state,
            last_synced_at: i.last_synced_at,
            error: i.error.clone(),
            pending: db.and_then(|d| d.sync_pending().ok()).unwrap_or(0),
            files: FilesStatus {
                pending: i.files.left + i64::try_from(i.files.touched.len()).unwrap_or(0),
                conflicts: i.files.conflicts.clone(),
                skipped: i.files.skipped.clone(),
                unavailable: i.files.unavailable,
            },
        }
    }

    pub fn files_unavailable(&self) -> bool {
        lock(&self.inner).files.unavailable
    }

    /// Signed out: nothing syncs and nothing is deleted.
    pub fn set_off(&self) {
        let mut i = lock(&self.inner);
        i.state = SyncState::Off;
        i.error = None;
        i.identity = None;
        i.files = FilesState::default();
    }

    fn set(&self, state: SyncState, error: Option<String>) {
        let mut i = lock(&self.inner);
        i.state = state;
        i.error = error;
    }

    /// One sync with the given token (`None`: signed out). A run already in progress
    /// is not doubled: its status is returned as it is. `files`: also sync the notes folder.
    pub async fn run(
        &self,
        db: &AppDb,
        base: &str,
        token: Result<Option<String>, String>,
        files: Option<&FilesEnv<'_>>,
    ) -> RunResult {
        let done = |core: &SyncCore, applied, days_changed, retry| RunResult {
            status: core.status(Some(db)),
            applied,
            days_changed,
            retry,
        };
        let token = match token {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.set_off();
                return done(self, Applied::default(), Vec::new(), false);
            }
            Err(e) => {
                self.set(SyncState::Error, Some(e));
                return done(self, Applied::default(), Vec::new(), false);
            }
        };
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return done(self, Applied::default(), Vec::new(), false);
        }
        self.set(SyncState::Syncing, None);
        let mut out = FilesOutcome::default();
        let result = self.sync(db, base, &token, files, &mut out).await;
        self.running.store(false, Ordering::SeqCst);
        let days = std::mem::take(&mut out.days_changed);
        match result {
            Ok(applied) if out.failed.is_empty() => {
                lock(&self.inner).last_synced_at = Some(system_ms());
                self.set(SyncState::Idle, None);
                done(self, applied, days, false)
            }
            Ok(applied) => {
                // The rest synced; some notes files could not.
                let first = out.failed[0].clone();
                let n = out.failed.len();
                let more = if n > 1 {
                    format!(" (+{} more)", n - 1)
                } else {
                    String::new()
                };
                self.set(
                    SyncState::Error,
                    Some(format!("Couldn't sync some notes: {first}{more}")),
                );
                done(self, applied, days, false)
            }
            Err(e) => {
                let state = if matches!(e, SyncError::Offline(_)) {
                    SyncState::Offline
                } else {
                    SyncState::Error
                };
                self.set(state, Some(e.to_string()));
                done(self, Applied::default(), days, e.retryable())
            }
        }
    }

    async fn sync(
        &self,
        db: &AppDb,
        base: &str,
        token: &str,
        files: Option<&FilesEnv<'_>>,
        out: &mut FilesOutcome,
    ) -> Result<Applied, SyncError> {
        let cached = lock(&self.inner)
            .identity
            .as_ref()
            .filter(|(t, _)| t == token)
            .map(|(_, user)| user.clone());
        let user = match cached {
            Some(u) => u,
            None => {
                let u = self.client.identity(base, token).await?;
                lock(&self.inner).identity = Some((token.to_string(), u.clone()));
                u
            }
        };
        let applied = sync_once(db, &self.client, base, token, &user)
            .await
            .map(|o| o.applied)?;
        if let Some(f) = files {
            let result = self.sync_notes(db, base, token, &user, f, out).await;
            match result {
                Err(SyncError::Unavailable(_)) => {
                    let mut i = lock(&self.inner);
                    i.files = FilesState {
                        unavailable: true,
                        ..FilesState::default()
                    };
                }
                other => {
                    let copies = file_sync::walk(f.root).copies;
                    let mut i = lock(&self.inner);
                    i.files.unavailable = false;
                    i.files.left = i64::try_from(out.pending).unwrap_or(0);
                    i.files.conflicts = copies;
                    i.files.skipped = out.skipped.clone();
                    drop(i);
                    other?;
                }
            }
        }
        Ok(applied)
    }

    async fn sync_notes(
        &self,
        db: &AppDb,
        base: &str,
        token: &str,
        user: &str,
        f: &FilesEnv<'_>,
        out: &mut FilesOutcome,
    ) -> Result<(), SyncError> {
        let remote = HttpRemote {
            client: &self.client,
            base,
            token,
        };
        let key = file_sync::resolve_key(f.keys, &remote, user).await?;
        lock(&self.inner).files.touched.clear();
        let env = Env {
            root: f.root,
            store: f.store,
            db,
            key: &key,
            user,
            stamp: &f.stamp,
        };
        file_sync::sync_files_into(&env, &remote, out).await
    }

    /// The watcher saw `rel` (a syncable notes file) change. `true` when it is new to sync.
    fn touch(&self, rel: &str) -> bool {
        lock(&self.inner).files.touched.insert(rel.to_string())
    }
}

/// Why a sync runs: decides whether the notes folder is part of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// App start and every 5 min: everything, even a server that said "not configured".
    Periodic,
    /// The "Sync now" button.
    Manual,
    /// A goal/tag/project changed: the app database only.
    Data,
    /// A notes file changed, or a retry after a failure: skipped while the server
    /// says it has no notes storage (that is only re-checked on the slow schedule).
    Files,
}

/// Status plus the timers that decide when to sync.
pub struct SyncService {
    core: SyncCore,
    debounce_gen: AtomicU64,
    files_gen: AtomicU64,
    retry_gen: AtomicU64,
    backoff: AtomicU64,
}

impl SyncService {
    pub fn new() -> Result<SyncService, String> {
        Ok(SyncService {
            core: SyncCore::new()?,
            debounce_gen: AtomicU64::new(0),
            files_gen: AtomicU64::new(0),
            retry_gen: AtomicU64::new(0),
            backoff: AtomicU64::new(BACKOFF_START),
        })
    }

    /// Signed out from the app: stop syncing and tell the UI.
    pub fn signed_out(&self, app: &AppHandle) {
        self.retry_gen.fetch_add(1, Ordering::SeqCst);
        self.files_gen.fetch_add(1, Ordering::SeqCst);
        self.core.set_off();
        let db = app.state::<AppDbHandle>();
        report(app, self.core.status(db.get().ok().as_deref()));
    }
}

fn report(app: &AppHandle, status: SyncStatus) {
    if let Err(e) = app.emit("sync-status", &status) {
        eprintln!("emitting sync-status: {e}");
    }
}

fn emit_changed(app: &AppHandle, event: &str) {
    if let Err(e) = app.emit(event, serde_json::json!({})) {
        eprintln!("emitting {event}: {e}");
    }
}

/// Runs one sync and tells the frontend what happened.
async fn run_and_report(app: &AppHandle, svc: &Arc<SyncService>, trigger: Trigger) -> SyncStatus {
    let handle = app.state::<AppDbHandle>();
    let Ok(db) = handle.get() else {
        return svc.core.status(None);
    };
    let token = tauri::async_runtime::spawn_blocking(account::get_token)
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    report(
        app,
        SyncStatus {
            state: SyncState::Syncing,
            ..svc.core.status(Some(&db))
        },
    );
    let was_off = token.as_ref().is_ok_and(Option::is_none);
    let with_files = match trigger {
        Trigger::Periodic | Trigger::Manual => true,
        Trigger::Data => false,
        Trigger::Files => !svc.core.files_unavailable(),
    };
    let store = app.state::<Arc<DayStore>>();
    let keys = account::KeychainNotesKey;
    let env = FilesEnv {
        root: store.root(),
        store: &store,
        keys: &keys,
        stamp: jiff::Zoned::now().strftime("%Y-%m-%d %H%M").to_string(),
    };
    let result = svc
        .core
        .run(
            &db,
            &account::server_url(),
            token,
            with_files.then_some(&env),
        )
        .await;
    for changed in &result.days_changed {
        if let Err(e) = app.emit("day-changed", changed) {
            eprintln!("emitting day-changed: {e}");
        }
    }
    if result.applied.tags {
        emit_changed(app, "tags-changed");
    }
    if result.applied.calendars {
        emit_changed(app, "calendars-changed");
    }
    if result.applied.projects {
        emit_changed(app, "projects-changed");
    }
    if result.applied.goals {
        emit_changed(app, "goals-changed");
    }
    if result.retry {
        schedule_retry(app, svc);
    } else {
        svc.retry_gen.fetch_add(1, Ordering::SeqCst);
        if !was_off {
            svc.backoff.store(BACKOFF_START, Ordering::SeqCst);
        }
    }
    report(app, result.status.clone());
    result.status
}

/// Tries again after 30 s, doubling up to 5 min while it keeps failing.
fn schedule_retry(app: &AppHandle, svc: &Arc<SyncService>) {
    let generation = svc.retry_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let wait = svc.backoff.load(Ordering::SeqCst);
    svc.backoff
        .store((wait * 2).min(BACKOFF_MAX), Ordering::SeqCst);
    let (app, svc) = (app.clone(), svc.clone());
    later(Duration::from_secs(wait), async move {
        if svc.retry_gen.load(Ordering::SeqCst) == generation {
            run_and_report(&app, &svc, Trigger::Files).await;
        }
    });
}

/// Runs `f` after `wait`. A sleeping thread keeps this free of a direct tokio dependency.
fn later(wait: Duration, f: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(wait);
        tauri::async_runtime::block_on(f);
    });
}

/// App start: sync now (when signed in), then every 5 min; 5 s after any local change
/// to the app database.
pub fn start(app: &AppHandle) {
    let svc = app.state::<Arc<SyncService>>().inner().clone();
    {
        let (app, svc) = (app.clone(), svc.clone());
        std::thread::spawn(move || {
            loop {
                tauri::async_runtime::block_on(run_and_report(&app, &svc, Trigger::Periodic));
                std::thread::sleep(PERIOD);
            }
        });
    }
    for event in CHANGE_EVENTS {
        let (handle, svc) = (app.clone(), svc.clone());
        app.listen(event, move |_| {
            let generation = svc.debounce_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let (handle, svc) = (handle.clone(), svc.clone());
            later(DEBOUNCE, async move {
                if svc.debounce_gen.load(Ordering::SeqCst) != generation || !svc.core.signed_in() {
                    return;
                }
                let pending = handle
                    .state::<AppDbHandle>()
                    .get()
                    .ok()
                    .and_then(|db| db.sync_pending().ok())
                    .unwrap_or(0);
                if pending > 0 {
                    run_and_report(&handle, &svc, Trigger::Data).await;
                }
            });
        });
    }
}

/// The folder watcher saw `path` change. A syncable notes file whose content is not what
/// was last synced (so not the engine's own write) schedules a sync 10 s later.
pub fn notes_path_changed(app: &AppHandle, path: &Path) {
    let Some(svc) = app.try_state::<Arc<SyncService>>() else {
        return;
    };
    if !svc.core.signed_in() {
        return;
    }
    let store = app.state::<Arc<DayStore>>();
    let Some(rel) = relative_syncable(store.root(), path) else {
        return;
    };
    let Ok(db) = app.state::<AppDbHandle>().get() else {
        return;
    };
    let current = std::fs::read(path)
        .ok()
        .map(|b| crate::notes_crypto::rev_of(&b));
    if current == db.file_synced(&rel).ok().flatten() {
        return;
    }
    svc.core.touch(&rel);
    let svc = svc.inner().clone();
    let generation = svc.files_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = app.clone();
    later(FILES_DEBOUNCE, async move {
        if svc.files_gen.load(Ordering::SeqCst) == generation && svc.core.signed_in() {
            run_and_report(&handle, &svc, Trigger::Files).await;
        }
    });
}

/// `path` relative to the notes folder (forward slashes) if it is one of the synced files.
fn relative_syncable(root: &Path, path: &Path) -> Option<String> {
    let rel = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    file_sync::classify(&rel).map(|_| rel)
}

#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    svc: State<'_, Arc<SyncService>>,
) -> Result<SyncStatus, String> {
    let svc = svc.inner().clone();
    // A manual sync starts the backoff over.
    svc.backoff.store(BACKOFF_START, Ordering::SeqCst);
    Ok(run_and_report(&app, &svc, Trigger::Manual).await)
}

#[tauri::command]
pub fn sync_status(svc: State<Arc<SyncService>>, db: State<AppDbHandle>) -> SyncStatus {
    svc.core.status(db.get().ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::{clock, dev, draft};
    use crate::sync_client::test_util::{Store, serve, store_server};

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    fn token(t: &str) -> Result<Option<String>, String> {
        Ok(Some(t.to_string()))
    }

    #[test]
    fn signed_out_never_talks_to_the_server_and_reports_off() {
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        let (base, seen) = serve(|_, _, _| (500, String::new()));
        let core = SyncCore::new().unwrap();
        let r = run(core.run(&d.db, &base, Ok(None), None));
        assert_eq!(r.status.state, SyncState::Off);
        assert_eq!(r.status.pending, 1, "pending changes are kept for later");
        assert!(seen.lock().unwrap().is_empty());
        assert!(!core.signed_in());
    }

    #[test]
    fn success_is_idle_with_a_time_and_failures_map_to_states() {
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        let (base, _) = store_server(Arc::new(Mutex::new(Store::new(500))));
        let core = SyncCore::new().unwrap();
        let r = run(core.run(&d.db, &base, token("tok"), None));
        assert_eq!(r.status.state, SyncState::Idle);
        assert!(r.status.last_synced_at.is_some() && r.status.error.is_none());
        assert_eq!(r.status.pending, 0);
        assert!(!r.retry && core.signed_in());
        // Wrong token: 401 on the account lookup, "Sign in again", no retry, data kept.
        d.db.tag_save("b", "sun", 1).unwrap();
        let r = run(core.run(&d.db, &base, token("old"), None));
        assert_eq!(r.status.state, SyncState::Error);
        assert_eq!(r.status.error.as_deref(), Some("Sign in again"));
        assert!(!r.retry);
        assert_eq!(d.db.saved_tags().unwrap().len(), 2);
        assert_eq!(r.status.pending, 1);
        // Offline.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
        };
        let r = run(core.run(&d.db, &closed, token("tok2"), None));
        assert_eq!(r.status.state, SyncState::Offline);
        assert!(r.retry);
        assert_eq!(r.status.pending, 1);
        // Back online with the first token again.
        let r = run(core.run(&d.db, &base, token("tok"), None));
        assert_eq!(r.status.state, SyncState::Idle);
        assert_eq!(r.status.pending, 0);
    }

    #[test]
    fn a_pull_reports_what_it_changed() {
        let store = Arc::new(Mutex::new(Store::new(500)));
        let a = dev(&clock());
        a.db.goal_create(&draft("Rust", &["rust"], None), 1)
            .unwrap();
        let (base, _) = store_server(store);
        let core = SyncCore::new().unwrap();
        run(core.run(&a.db, &base, token("tok"), None));
        let b = dev(&clock());
        let r = run(SyncCore::new()
            .unwrap()
            .run(&b.db, &base, token("tok"), None));
        assert!(r.applied.goals && r.applied.tags && !r.applied.projects);
        let again = run(SyncCore::new()
            .unwrap()
            .run(&b.db, &base, token("tok"), None));
        assert!(!again.applied.any());
    }

    #[test]
    fn status_json_shape() {
        let s = SyncStatus {
            state: SyncState::Offline,
            last_synced_at: Some(5),
            error: None,
            pending: 2,
            files: FilesStatus {
                pending: 1,
                conflicts: vec!["config (conflict 2026-10-22 1530).json".into()],
                skipped: vec![],
                unavailable: false,
            },
        };
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            serde_json::json!({"state":"offline","lastSyncedAt":5,"error":null,"pending":2,
                "files":{"pending":1,"conflicts":["config (conflict 2026-10-22 1530).json"],
                         "skipped":[],"unavailable":false}})
        );
    }

    struct Mem(Mutex<Option<String>>);

    impl KeyStore for Mem {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(lock(&self.0).clone())
        }
        fn set(&self, v: &str) -> Result<(), String> {
            *lock(&self.0) = Some(v.into());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            *lock(&self.0) = None;
            Ok(())
        }
    }

    fn notes_dir() -> (crate::config::test_util::TempDir, DayStore) {
        let tmp = crate::config::test_util::TempDir::new();
        let store = DayStore::new(std::fs::canonicalize(&tmp.0).unwrap());
        (tmp, store)
    }

    /// App sync answered by a real store; notes routes by `notes`.
    fn server_with(
        notes: impl Fn(&str) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Store>>) {
        let store = Arc::new(Mutex::new(Store::new(500)));
        let s = store.clone();
        let (base, _) = serve(move |line, _, body| {
            if line.starts_with("GET /api/me") {
                (200, r#"{"id":"u1","email":"a@b.c"}"#.into())
            } else if line.contains("/api/sync") {
                lock(&s).handle(body)
            } else {
                notes(line)
            }
        });
        (base, store)
    }

    #[test]
    fn a_server_without_notes_storage_keeps_app_sync_working() {
        let (_tmp, notes) = notes_dir();
        let keys = Mem(Mutex::new(None));
        let (base, _) = server_with(|_| {
            (
                503,
                r#"{"error":"Notes sync isn't configured on this server"}"#.into(),
            )
        });
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        let core = SyncCore::new().unwrap();
        let env = FilesEnv {
            root: notes.root(),
            store: &notes,
            keys: &keys,
            stamp: "s".into(),
        };
        let r = run(core.run(&d.db, &base, token("tok"), Some(&env)));
        assert_eq!(r.status.state, SyncState::Idle, "{:?}", r.status);
        assert!(r.status.error.is_none() && !r.retry);
        assert!(r.status.files.unavailable);
        assert_eq!(r.status.pending, 0, "the app data did sync");
        assert!(core.files_unavailable());
        // Signing out forgets it.
        core.set_off();
        assert!(!core.files_unavailable());
    }

    #[test]
    fn a_notes_pass_reports_conflict_copies_and_caches_the_key() {
        let (_tmp, notes) = notes_dir();
        std::fs::write(
            notes.root().join("config (conflict 2026-10-22 1530).json"),
            "{}",
        )
        .unwrap();
        let keys = Mem(Mutex::new(None));
        let (base, _) = server_with(|line| {
            if line.contains("/api/notes/key") {
                (
                    200,
                    r#"{"key":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk="}"#.into(),
                )
            } else {
                (200, r#"{"cursor":0,"hasMore":false,"changes":[]}"#.into())
            }
        });
        let d = dev(&clock());
        let core = SyncCore::new().unwrap();
        let env = FilesEnv {
            root: notes.root(),
            store: &notes,
            keys: &keys,
            stamp: "s".into(),
        };
        let r = run(core.run(&d.db, &base, token("tok"), Some(&env)));
        assert_eq!(r.status.state, SyncState::Idle, "{:?}", r.status);
        assert_eq!(
            r.status.files,
            FilesStatus {
                pending: 0,
                conflicts: vec!["config (conflict 2026-10-22 1530).json".into()],
                skipped: vec![],
                unavailable: false
            }
        );
        assert!(keys.get().unwrap().unwrap().starts_with("u1:"));
        // Without a files env only the app database syncs.
        let r = run(core.run(&d.db, &base, token("tok"), None));
        assert_eq!(r.status.state, SyncState::Idle);
    }

    #[test]
    fn only_synced_file_shapes_are_relative_syncable() {
        let root = Path::new("/n");
        let rel = |p: &str| relative_syncable(root, Path::new(p));
        assert_eq!(
            rel("/n/2026/10/2026-10-20.md").as_deref(),
            Some("2026/10/2026-10-20.md")
        );
        assert_eq!(rel("/n/config.json").as_deref(), Some("config.json"));
        assert_eq!(rel("/n/2026/10/2026-10-20 (conflict x).md"), None);
        assert!(rel("/n/2026/10/2026-10-20 (conflict 2026-10-22 0930) 2.md").is_some());
        assert_eq!(rel("/elsewhere/config.json"), None);
        assert_eq!(rel("/n/2026/10/.2026-10-20.md.catenga-tmp-1-0"), None);
    }
}
