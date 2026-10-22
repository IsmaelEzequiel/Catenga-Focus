mod account;
mod app_db;
mod atomic;
mod commands;
mod config;
mod dashboard;
mod day_file;
mod file_sync;
mod goals;
mod google;
mod google_client;
mod google_commands;
mod index;
mod lifecycle;
mod loopback;
mod mirror;
mod notes_crypto;
mod recovery;
mod search;
mod sessions;
mod settings;
mod storage;
mod sync;
mod sync_client;
mod sync_commands;
mod tags;
mod timer;
mod timer_commands;
mod timer_service;
mod tray;
mod tray_model;
mod watcher;

use commands::IndexHandle;
use index::{Index, IndexEvent};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use storage::DayStore;
use tauri::{Emitter, Manager};

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `CATENGA_NOTES_DIR` (dev override), else the folder chosen in Settings, else `~/Catenga/`.
fn notes_root(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let (root, _) = settings::resolve(
        std::env::var_os("CATENGA_NOTES_DIR"),
        &app.path().app_data_dir()?,
        &app.path().home_dir()?,
    );
    Ok(root)
}

/// `CATENGA_INDEX_DB` if set (dev/test override), else `<app cache dir>/index.sqlite`.
/// The index is a cache and must stay out of the (synced) notes folder.
fn index_db_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var_os("CATENGA_INDEX_DB") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(app.path().app_cache_dir()?.join("index.sqlite")),
    }
}

/// `CATENGA_APP_DB` if set (dev/test override), else `<app data dir>/catenga.sqlite`.
/// It holds projects, tags and goals (a source of truth), so it stays out of the
/// notes folder and is never reset automatically.
fn app_db_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var_os("CATENGA_APP_DB") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(app.path().app_data_dir()?.join("catenga.sqlite")),
    }
}

/// `CATENGA_TIMER_JSON` if set (dev/test override), else `<app data dir>/timer.json`.
/// The running-timer pointer is machine state and must stay out of the notes folder.
fn timer_json_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var_os("CATENGA_TIMER_JSON") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(app.path().app_data_dir()?.join("timer.json")),
    }
}

/// `CATENGA_RECOVERY_DIR` if set (dev/test override), else `<app data dir>/recovery`.
/// Unsaved-note copies stay out of the notes folder (not synced, not indexed).
fn recovery_dir_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var_os("CATENGA_RECOVERY_DIR") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(app.path().app_data_dir()?.join("recovery")),
    }
}

/// Full reconciliation, reporting progress to the frontend.
fn sync_with_events(index: &Index, handle: &tauri::AppHandle) {
    index.sync_all_with(&|status| {
        if let Err(e) = handle.emit("index-status", status) {
            eprintln!("emitting index-status: {e}");
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::SignInState::default())
        .manage(google_commands::GoogleState::new())
        .manage(lifecycle::QuitState::default())
        .setup(|app| {
            let root = notes_root(app)?;
            std::fs::create_dir_all(&root)?;
            // Canonical so watcher-reported paths (e.g. /private/var on macOS) share the prefix.
            let root = std::fs::canonicalize(&root)?;
            atomic::cleanup_stale_temps(&root);
            let store = Arc::new(DayStore::new(root.clone()));
            app.manage(store.clone());
            app.manage(recovery::RecoveryDir(recovery_dir_path(app)?));
            let timer = timer_service::TimerService::new(
                root.clone(),
                timer_json_path(app)?,
                store.clone(),
                Arc::new(timer_commands::TauriSink(app.handle().clone())),
                Arc::new(jiff::Timestamp::now),
                jiff::tz::TimeZone::system(),
            );
            timer.recover();
            // Runs for the life of the app; the process exit ends it.
            drop(timer.spawn());
            app.manage(timer);
            // Without the app database the app still works; only projects, tags and
            // goals report why they are off. The file is never touched on failure.
            let app_db = app_db_path(app)
                .map_err(|e| e.to_string())
                .and_then(|path| app_db::AppDb::open(&path))
                .map(Arc::new);
            match &app_db {
                Ok(db) => {
                    // Sync rows belong to one notes folder; a different one starts over
                    // (never a delete) before any sync pass can run.
                    if let Err(e) = db.files_adopt_root(&root.to_string_lossy()) {
                        eprintln!("resetting notes sync state: {e}");
                    }
                    let now = commands::now_secs();
                    if let Err(e) = db.import_config_tag_colors(&root, now) {
                        eprintln!("importing tag colors: {e}");
                    }
                    if let Err(e) = db.import_goals_json(&root, now) {
                        eprintln!("importing goals: {e}");
                    }
                }
                Err(e) => eprintln!("app database disabled: {e}"),
            }
            app.manage(commands::AppDbHandle(app_db));
            // Sync is optional: without its HTTP client the app just never syncs.
            match sync_commands::SyncService::new() {
                Ok(service) => {
                    app.manage(Arc::new(service));
                    sync_commands::start(app.handle());
                }
                Err(e) => eprintln!("sync disabled: {e}"),
            }
            google_commands::refresh_projects_in_background(app.handle());
            // Without the index the app still works; only search and history are off.
            let index = index_db_path(app)
                .map_err(|e| e.to_string())
                .and_then(|db| Index::open(&db, &root))
                .map(Arc::new);
            if let Err(e) = &index {
                eprintln!("search index disabled: {e}");
            }
            if let Ok(index) = &index {
                let (index, handle) = (index.clone(), app.handle().clone());
                std::thread::spawn(move || sync_with_events(&index, &handle));
            }
            let on_index = {
                let (index, handle) = (index.clone(), app.handle().clone());
                move |event: IndexEvent| {
                    let Ok(index) = &index else { return };
                    match event {
                        IndexEvent::Day(date) => {
                            if let Err(e) = index.reindex_day(&date) {
                                eprintln!("indexing {date}: {e}");
                            }
                        }
                        IndexEvent::Sessions(date) => {
                            if let Err(e) = index.reindex_sessions(&date) {
                                eprintln!("indexing sessions of {date}: {e}");
                            }
                            // Tells an open dashboard the index now has the new sessions.
                            if let Err(e) = handle.emit("index-status", index.status()) {
                                eprintln!("emitting index-status: {e}");
                            }
                        }
                        IndexEvent::Rescan => sync_with_events(index, &handle),
                    }
                }
            };
            app.manage(IndexHandle(index));
            // Without a watcher the app still works; only live reload of external edits is off.
            let handle = app.handle().clone();
            let on_change = move |changed| {
                if let Err(e) = handle.emit("day-changed", changed) {
                    eprintln!("emitting day-changed: {e}");
                }
            };
            let sync_handle = app.handle().clone();
            let on_path = move |path: &std::path::Path| {
                sync_commands::notes_path_changed(&sync_handle, path);
            };
            match watcher::start(store, watcher::DEBOUNCE, on_change, on_index, on_path) {
                Ok(debouncer) => {
                    app.manage(Mutex::new(debouncer));
                }
                Err(e) => eprintln!("folder watching disabled: {e}"),
            }
            // Without the menu bar item the app still works; closing the window
            // would then leave no way back except the Dock, so say so.
            if let Err(e) = tray::start(app.handle()) {
                eprintln!("menu bar item disabled: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_version,
            lifecycle::app_hide,
            lifecycle::app_show,
            lifecycle::app_exit,
            lifecycle::app_cancel_quit,
            settings::notes_dir_info,
            settings::set_notes_dir,
            settings::app_restart,
            commands::load_config,
            commands::load_day,
            commands::save_day,
            recovery::recovery_put,
            recovery::recovery_list,
            recovery::recovery_drop,
            commands::search,
            commands::history,
            commands::dashboard,
            commands::goals,
            commands::goals_archived,
            commands::goal_create,
            commands::goal_update,
            commands::goal_set_archived,
            commands::goal_delete,
            commands::calendars,
            commands::calendar_set_shown,
            commands::projects,
            commands::project_set_archived,
            commands::tags,
            commands::tag_save,
            commands::tag_delete,
            commands::index_status,
            commands::server_url,
            commands::sign_in,
            commands::cancel_sign_in,
            commands::get_account_token,
            commands::set_account_token,
            commands::clear_account_token,
            sync_commands::sync_now,
            sync_commands::sync_status,
            google_commands::google_status,
            google_commands::google_connect,
            google_commands::google_cancel_connect,
            google_commands::google_disconnect,
            google_commands::calendar_day,
            google_commands::calendar_refresh,
            google_commands::calendar_create,
            google_commands::calendar_update,
            google_commands::calendar_delete,
            google_commands::projects_refresh,
            google_commands::project_from_event,
            timer_commands::timer_status,
            timer_commands::timer_start,
            timer_commands::timer_stop,
            timer_commands::timer_skip_break,
            timer_commands::timer_resolve_stale,
            timer_commands::timer_sessions,
            timer_commands::timer_delete_session,
            timer_commands::timer_relink
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| lifecycle::on_run_event(app, &event));
}
