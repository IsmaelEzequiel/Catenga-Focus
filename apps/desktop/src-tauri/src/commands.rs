use crate::account;
use crate::app_db::{AppDb, Project};
use crate::config::{self, Config};
use crate::dashboard::{DashRange, Dashboard};
use crate::day_file::Block;
use crate::goals::{GoalDraft, GoalProgress};
use crate::index::{Index, IndexStatus};
use crate::loopback;
use crate::search::{HistoryKind, HistoryResult, SearchResponse};
use crate::storage::{DayStore, LoadedDay, SaveOutcome};
use crate::tags::Tag;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

/// The search index, or why it could not be opened. The app works without it.
pub struct IndexHandle(pub Result<Arc<Index>, String>);

impl IndexHandle {
    fn get(&self) -> Result<Arc<Index>, String> {
        self.0
            .clone()
            .map_err(|e| format!("search index unavailable: {e}"))
    }
}

/// The app database, or why it could not be opened. The app works without it.
pub struct AppDbHandle(pub Result<Arc<AppDb>, String>);

impl AppDbHandle {
    pub fn get(&self) -> Result<Arc<AppDb>, String> {
        self.0
            .clone()
            .map_err(|e| format!("Couldn't open the app database: {e}"))
    }
}

/// `config.json`, with the tag colors taken from the database (which imported
/// them from the file once). Without the database the file's colors are shown.
#[tauri::command]
pub fn load_config(store: State<Arc<DayStore>>, db: State<AppDbHandle>) -> Result<Config, String> {
    let mut config = config::load_or_create(store.root())?;
    if let Ok(db) = &db.0 {
        if let Err(e) = db.import_config_tag_colors(store.root(), now_secs()) {
            eprintln!("importing tag colors: {e}");
        }
        match db.tag_colors() {
            Ok(colors) => config.tag_colors = colors,
            Err(e) => eprintln!("reading tag colors: {e}"),
        }
    }
    Ok(config)
}

#[tauri::command]
pub fn load_day(store: State<Arc<DayStore>>, date: String) -> Result<LoadedDay, String> {
    store.load(&date)
}

#[tauri::command]
pub fn save_day(
    store: State<Arc<DayStore>>,
    index: State<IndexHandle>,
    date: String,
    blocks: Vec<Block>,
    base_rev: String,
) -> Result<SaveOutcome, String> {
    let outcome = store.save(&date, &blocks, &base_rev)?;
    // A save never fails because of the index; the watcher also reindexes.
    if let Ok(index) = &index.0
        && let Err(e) = index.reindex_day(&date)
    {
        eprintln!("indexing {date}: {e}");
    }
    Ok(outcome)
}

// Index reads are async so they run off the main thread.

#[tauri::command]
pub async fn search(
    index: State<'_, IndexHandle>,
    query: String,
) -> Result<SearchResponse, String> {
    let index = index.get()?;
    tauri::async_runtime::spawn_blocking(move || index.search(&query))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn history(
    index: State<'_, IndexHandle>,
    kind: HistoryKind,
    value: String,
) -> Result<HistoryResult, String> {
    let index = index.get()?;
    tauri::async_runtime::spawn_blocking(move || index.history(kind, &value))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn dashboard(
    index: State<'_, IndexHandle>,
    range: DashRange,
) -> Result<Dashboard, String> {
    let index = index.get()?;
    // Local calendar date in the system zone; the pure code takes it as input.
    let today = jiff::Zoned::now().date();
    tauri::async_runtime::spawn_blocking(move || index.dashboard(range, today))
        .await
        .map_err(|e| e.to_string())?
}

/// Unix seconds, for the `created_at` / `updated_at` columns.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn emit(app: &AppHandle, event: &str) {
    if let Err(e) = app.emit(event, serde_json::json!({})) {
        eprintln!("emitting {event}: {e}");
    }
}

/// Imports the old `goals.json` if needed, then computes progress from the index.
fn goal_list(
    root: &std::path::Path,
    db: &AppDb,
    index: &IndexHandle,
    archived: bool,
) -> Result<Vec<GoalProgress>, String> {
    db.import_goals_json(root, now_secs())?;
    let views = db.goal_views(archived)?;
    let today = jiff::Zoned::now().date();
    index.get()?.goals(&views, today)
}

/// Saved tags merged with the tags seen in notes (those are skipped if the index is down).
fn tag_list(db: &AppDb, index: &IndexHandle) -> Result<Vec<Tag>, String> {
    let seen = index
        .0
        .as_ref()
        .ok()
        .and_then(|i| {
            i.all_tags()
                .map_err(|e| eprintln!("reading tags: {e}"))
                .ok()
        })
        .unwrap_or_default();
    db.tags(&seen)
}

#[tauri::command]
pub async fn goals(
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
) -> Result<Vec<GoalProgress>, String> {
    let (root, db) = (store.root().to_path_buf(), db.get()?);
    let index = IndexHandle(index.0.clone());
    tauri::async_runtime::spawn_blocking(move || goal_list(&root, &db, &index, false))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn goals_archived(
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
) -> Result<Vec<GoalProgress>, String> {
    let (root, db) = (store.root().to_path_buf(), db.get()?);
    let index = IndexHandle(index.0.clone());
    tauri::async_runtime::spawn_blocking(move || goal_list(&root, &db, &index, true))
        .await
        .map_err(|e| e.to_string())?
}

/// Runs a goal write, tells the frontend, and returns the fresh active list.
async fn goal_write(
    app: AppHandle,
    store: &DayStore,
    db: &AppDbHandle,
    index: &IndexHandle,
    write: impl FnOnce(&AppDb) -> Result<bool, String> + Send + 'static,
) -> Result<Vec<GoalProgress>, String> {
    let (root, db) = (store.root().to_path_buf(), db.get()?);
    let index = IndexHandle(index.0.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let tags_created = write(&db)?;
        emit(&app, "goals-changed");
        if tags_created {
            emit(&app, "tags-changed");
        }
        goal_list(&root, &db, &index, false)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn goal_create(
    app: AppHandle,
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
    draft: GoalDraft,
) -> Result<Vec<GoalProgress>, String> {
    goal_write(app, &store, &db, &index, move |db| {
        db.goal_create(&draft, now_secs()).map(|w| w.tags_created)
    })
    .await
}

#[tauri::command]
pub async fn goal_update(
    app: AppHandle,
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
    id: i64,
    draft: GoalDraft,
) -> Result<Vec<GoalProgress>, String> {
    goal_write(app, &store, &db, &index, move |db| {
        db.goal_edit(id, &draft, now_secs())
    })
    .await
}

#[tauri::command]
pub async fn goal_set_archived(
    app: AppHandle,
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
    id: i64,
    archived: bool,
) -> Result<Vec<GoalProgress>, String> {
    goal_write(app, &store, &db, &index, move |db| {
        db.goal_set_archived(id, archived, now_secs())
            .map(|()| false)
    })
    .await
}

#[tauri::command]
pub async fn goal_delete(
    app: AppHandle,
    store: State<'_, Arc<DayStore>>,
    db: State<'_, AppDbHandle>,
    index: State<'_, IndexHandle>,
    id: i64,
) -> Result<Vec<GoalProgress>, String> {
    goal_write(app, &store, &db, &index, move |db| {
        db.goal_delete(id).map(|()| false)
    })
    .await
}

// ------------------------------------------------------------ projects & tags

#[tauri::command]
pub fn projects(db: State<AppDbHandle>) -> Result<Vec<Project>, String> {
    db.get()?.projects()
}

#[tauri::command]
pub fn project_set_shown(
    app: AppHandle,
    db: State<AppDbHandle>,
    id: i64,
    shown: bool,
) -> Result<Vec<Project>, String> {
    let db = db.get()?;
    db.set_shown(id, shown, now_secs())?;
    emit(&app, "projects-changed");
    db.projects()
}

#[tauri::command]
pub fn tags(db: State<AppDbHandle>, index: State<IndexHandle>) -> Result<Vec<Tag>, String> {
    tag_list(&*db.get()?, &index)
}

#[tauri::command]
pub fn tag_save(
    app: AppHandle,
    db: State<AppDbHandle>,
    index: State<IndexHandle>,
    name: String,
    color: String,
) -> Result<Vec<Tag>, String> {
    let db = db.get()?;
    db.tag_save(&name, &color, now_secs())?;
    emit(&app, "tags-changed");
    tag_list(&db, &index)
}

#[tauri::command]
pub fn tag_delete(
    app: AppHandle,
    db: State<AppDbHandle>,
    index: State<IndexHandle>,
    id: i64,
) -> Result<Vec<Tag>, String> {
    let db = db.get()?;
    db.tag_delete(id)?;
    emit(&app, "tags-changed");
    tag_list(&db, &index)
}

#[tauri::command]
pub fn index_status(index: State<IndexHandle>) -> Result<IndexStatus, String> {
    Ok(index.get()?.status())
}

/// How long the browser sign-in may take before we stop listening.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Cancel flag of the sign-in wait in progress, if any.
#[derive(Default)]
pub struct SignInState(Mutex<Option<Arc<AtomicBool>>>);

/// PKCE challenge: 43 base64url characters (SHA-256 digest).
fn valid_challenge(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[tauri::command]
pub fn server_url() -> String {
    account::server_url()
}

/// Opens the server's login page in the browser and waits for the loopback callback.
/// Resolves with the one-time code; the frontend exchanges it for a token.
#[tauri::command]
pub async fn sign_in(
    app: AppHandle,
    state: State<'_, SignInState>,
    code_challenge: String,
) -> Result<String, String> {
    if !valid_challenge(&code_challenge) {
        return Err("invalid code challenge".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    // A new attempt replaces the previous one; its wait ends with "cancelled".
    if let Some(previous) = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .replace(cancel.clone())
    {
        previous.store(true, Ordering::SeqCst);
    }
    let (listener, port) = loopback::bind().map_err(|e| format!("starting sign-in: {e}"))?;
    let url = format!(
        "{}/login?app_port={port}&code_challenge={code_challenge}",
        account::server_url()
    );
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("opening the browser: {e}"))?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    let params = tauri::async_runtime::spawn_blocking(move || {
        loopback::wait_for_callback(&listener, deadline, &cancel).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    code_from_params(&params)
}

/// The one-time app code from the callback, or why it is not usable.
fn code_from_params(params: &HashMap<String, String>) -> Result<String, String> {
    if params.contains_key("error") {
        return Err("access denied".into());
    }
    match params.get("code") {
        Some(code) if valid_code(code) => Ok(code.clone()),
        _ => Err("invalid sign-in code".into()),
    }
}

/// App codes are base64url.
fn valid_code(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[tauri::command]
pub fn cancel_sign_in(state: State<SignInState>) {
    if let Ok(mut current) = state.0.lock()
        && let Some(cancel) = current.take()
    {
        cancel.store(true, Ordering::SeqCst);
    }
}

#[tauri::command]
pub fn get_account_token() -> Result<Option<String>, String> {
    account::get_token()
}

#[tauri::command]
pub fn set_account_token(token: String) -> Result<(), String> {
    account::set_token(&token)
}

#[tauri::command]
pub fn clear_account_token() -> Result<(), String> {
    account::delete_token()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn sign_in_code_must_be_base64url() {
        assert_eq!(
            code_from_params(&params(&[("code", "abc-DEF_1")])),
            Ok("abc-DEF_1".to_string())
        );
        for bad in ["", "a b", "a.b", "a/b"] {
            assert!(
                code_from_params(&params(&[("code", bad)])).is_err(),
                "{bad}"
            );
        }
        assert!(code_from_params(&params(&[])).is_err());
    }

    #[test]
    fn sign_in_error_param_means_access_denied() {
        assert_eq!(
            code_from_params(&params(&[("error", "access_denied")])),
            Err("access denied".to_string())
        );
    }

    #[test]
    fn challenge_must_be_43_base64url_chars() {
        assert!(valid_challenge(
            "E9Melhoa2OwvFrEMTJguCHoeRZt8hn-a8_6sCjgJELE"
        ));
        assert!(!valid_challenge(""));
        assert!(!valid_challenge(
            "E9Melhoa2OwvFrEMTJguCHoeRZt8hn-a8_6sCjgJEL"
        ));
        assert!(!valid_challenge(
            "E9Melhoa2OwvFrEMTJguCHoeRZt8hn-a8_6sCjgJEL&"
        ));
        assert!(!valid_challenge(
            "E9Melhoa2OwvFrEMTJguCHoeRZt8hn-a8_6sCjgJELEE"
        ));
    }
}
